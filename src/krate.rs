use std::ascii::AsciiExt;
use std::cmp;
use std::collections::HashMap;
use std::io::prelude::*;
use std::io;
use std::iter::repeat;
use std::mem;
use std::sync::Arc;

use conduit::{Request, Response};
use conduit_router::RequestParams;
use curl::http;
use license_exprs;
use pg::GenericConnection;
use pg::rows::Row;
use pg::types::{ToSql, Slice};
use rustc_serialize::hex::ToHex;
use rustc_serialize::json;
use semver;
use time::{Timespec, Duration};
use url::{self, Url};
use yaqb::*;
use yaqb::query_builder::update;
use yaqb::types::structs::PgTimestamp;

use {Model, User, Keyword, Version};
use app::{App, RequestApp};
use db::RequestTransaction;
use dependency::{Dependency, EncodableDependency};
use download::{VersionDownload, EncodableVersionDownload};
use git;
use keyword::EncodableKeyword;
use model::{update_or_insert, UpdateOrInsert};
use upload;
use user::RequestUser;
use owner::{EncodableOwner, Owner, Rights, OwnerKind, Team, rights};
use util::errors::{NotFound, CargoError};
use util::{LimitErrorReader, HashingReader};
use util::{RequestUtils, CargoResult, internal, ChainError, human};
use version::{EncodableVersion, versions};

#[derive(Clone)]
pub struct Crate {
    pub id: i32,
    pub name: String,
    pub user_id: i32,
    pub updated_at: Timespec,
    pub created_at: Timespec,
    pub downloads: i32,
    pub max_version: semver::Version,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub documentation: Option<String>,
    pub readme: Option<String>,
    pub keywords: Vec<String>,
    pub license: Option<String>,
    pub repository: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct NewCrate<'a> {
    name: &'a str,
    user_id: i32,
    description: Option<&'a str>,
    homepage: Option<&'a str>,
    documentation: Option<&'a str>,
    readme: Option<&'a str>,
    keywords: Option<&'a str>,
    repository: Option<&'a str>,
    license: Option<&'a str>,
}

impl<'a> NewCrate<'a> {
    pub fn new(
        name: &'a str,
        user_id: i32,
        description: Option<&'a str>,
        homepage: Option<&'a str>,
        documentation: Option<&'a str>,
        readme: Option<&'a str>,
        keywords: Option<&'a str>,
        repository: Option<&'a str>,
        license: Option<&'a str>,
        license_file: Option<&'a str>,
    ) -> CargoResult<Self> {
        Ok(NewCrate {
            name: name,
            user_id: user_id,
            description: description,
            homepage: homepage,
            documentation: documentation,
            readme: readme,
            keywords: keywords,
            repository: repository,
            license: license,
        })
    }
}

table! {
    crates {
        id -> Serial,
        name -> VarChar,
        user_id -> Integer,
        updated_at -> Timestamp,
        created_at -> Timestamp,
        downloads -> Integer,
        max_version -> VarChar,
        description -> Nullable<VarChar>,
        homepage -> Nullable<VarChar>,
        documentation -> Nullable<VarChar>,
        readme -> Nullable<VarChar>,
        keywords -> Nullable<VarChar>,
        license -> Nullable<VarChar>,
        repository -> Nullable<VarChar>,
    }
}

insertable! {
    NewCrate<'a> => crates {
        name -> &'a str,
        user_id -> i32,
        description -> Option<&'a str>,
        homepage -> Option<&'a str>,
        documentation -> Option<&'a str>,
        readme -> Option<&'a str>,
        keywords -> Option<&'a str>,
        repository -> Option<&'a str>,
        license -> Option<&'a str>,
    }
}

table! {
    version_downloads {
        id -> Serial,
        version_id -> Integer,
        downloads -> Integer,
        counted -> Integer,
        date -> Timestamp,
        processed -> Bool,
    }
}

joinable!(crates -> versions (id = crate_id));
select_column_workaround!(crates -> versions (id, name, user_id, updated_at, created_at, downloads, max_version, description, homepage, documentation, readme, keywords, license, repository));
select_column_workaround!(versions -> crates (id, crate_id, num, updated_at, created_at, downloads, features, yanked));

use yaqb::expression::predicates::Eq;
use yaqb::expression::bound::Bound;

impl<'a> ::yaqb::query_builder::AsChangeset for NewCrate<'a> {
    type Changeset = (
        Eq<crates::name, Bound<types::VarChar, &'a str>>,
        Eq<crates::user_id, Bound<types::Integer, i32>>,
        Eq<crates::description, Bound<types::Nullable<types::VarChar>, Option<&'a str>>>,
        Eq<crates::homepage, Bound<types::Nullable<types::VarChar>, Option<&'a str>>>,
        Eq<crates::documentation, Bound<types::Nullable<types::VarChar>, Option<&'a str>>>,
        Eq<crates::readme, Bound<types::Nullable<types::VarChar>, Option<&'a str>>>,
        Eq<crates::keywords, Bound<types::Nullable<types::VarChar>, Option<&'a str>>>,
        Eq<crates::repository, Bound<types::Nullable<types::VarChar>, Option<&'a str>>>,
        Eq<crates::license, Bound<types::Nullable<types::VarChar>, Option<&'a str>>>,
    );

    fn as_changeset(self) -> Self::Changeset {
        (
            crates::name.eq(self.name),
            crates::user_id.eq(self.user_id),
            crates::description.eq(self.description),
            crates::homepage.eq(self.homepage),
            crates::documentation.eq(self.documentation),
            crates::readme.eq(self.readme),
            crates::keywords.eq(self.keywords),
            crates::repository.eq(self.repository),
            crates::license.eq(self.license),
        )
    }
}

struct NewVersionDownload {
    version_id: i32,
}

impl NewVersionDownload {
    fn new(version_id: i32) -> Self {
        NewVersionDownload {
            version_id: version_id,
        }
    }
}

insertable! {
    NewVersionDownload => version_downloads {
        version_id -> i32,
    }
}

const USEC_PER_SEC: i64 = 1_000_000;
const NSEC_PER_USEC: i64 = 1_000;

// Number of seconds from 1970-01-01 to 2000-01-01
const TIME_SEC_CONVERSION: i64 = 946684800;

fn parse_time(t: i64) -> Timespec {
    let mut sec = t / USEC_PER_SEC + TIME_SEC_CONVERSION;
    let mut usec = t % USEC_PER_SEC;

    if usec < 0 {
        sec -= 1;
        usec = USEC_PER_SEC + usec;
    }

    Timespec::new(sec, (usec * NSEC_PER_USEC) as i32)
}

impl Queriable<crates::SqlType> for Crate {
    type Row = (i32, String, i32, PgTimestamp, PgTimestamp, i32, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>);

    fn build(row: Self::Row) -> Self {
        let (id, name, user_id, updated_at, created_at, downloads, max_version, description, homepage, documentation, readme, keywords, license, repository) = row;
        let created_at = parse_time(created_at.0);
        let updated_at = parse_time(updated_at.0);
        let keywords = keywords.unwrap_or(String::new()).split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()).collect();
        let max_version = semver::Version::parse(&max_version).unwrap();

        Crate {
            id: id,
            name: name,
            user_id: user_id,
            updated_at: updated_at,
            created_at: created_at,
            downloads: downloads,
            max_version: max_version,
            description: description,
            homepage: homepage,
            documentation: documentation,
            readme: readme,
            keywords: keywords,
            license: license,
            repository: repository,
        }
    }
}

table! {
    metadata (total_downloads) {
        total_downloads -> BigInt,
    }
}

#[derive(RustcEncodable, RustcDecodable)]
pub struct EncodableCrate {
    pub id: String,
    pub name: String,
    pub updated_at: String,
    pub versions: Option<Vec<i32>>,
    pub created_at: String,
    pub downloads: i32,
    pub max_version: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub documentation: Option<String>,
    pub keywords: Vec<String>,
    pub license: Option<String>,
    pub repository: Option<String>,
    pub links: CrateLinks,
}

#[derive(RustcEncodable, RustcDecodable)]
pub struct CrateLinks {
    pub version_downloads: String,
    pub versions: Option<String>,
    pub owners: Option<String>,
    pub reverse_dependencies: String,
}

impl Crate {
    pub fn find_by_name(conn: &GenericConnection,
                        name: &str) -> CargoResult<Crate> {
        let stmt = try!(conn.prepare("SELECT * FROM crates \
                                      WHERE canon_crate_name(name) =
                                            canon_crate_name($1) LIMIT 1"));
        let row = try!(stmt.query(&[&name])).into_iter().next();
        let row = try!(row.chain_error(|| NotFound));
        Ok(Model::from_row(&row))
    }

    pub fn new_find_or_insert(conn: &Connection, new_crate: NewCrate, user_id: i32)
        -> CargoResult<Crate>
    {
        // TODO: like with users, this is sadly racy
        let target = crates::table.filter(canon_crate_name(crates::name)
                                          .eq(canon_crate_name(&new_crate.name)));
        conn.transaction(|| {
            match try!(update_or_insert(conn, target, &[new_crate])) {
                UpdateOrInsert::Updated(krate) => Ok(krate),
                UpdateOrInsert::Inserted(krate) => {
                    // Blacklist the current set of crates in the rust distribution
                    const RESERVED: &'static str = include_str!("reserved_crates.txt");
                    let krate: Crate = krate;

                    if RESERVED.lines().any(|line| krate.name == line) {
                        Err(human("cannot upload a crate with a reserved name"))
                    } else {
                        try!(krate.add_owner(conn, user_id));
                        Ok(krate)
                    }
                }
            }
        }).map_err(|e| match e {
            TransactionError::CouldntCreateTransaction(e) => e.into(),
            TransactionError::UserReturnedError(e) => e,
        })
    }

    fn add_owner(&self, conn: &Connection, user_id: i32) -> CargoResult<()> {
        try!(conn.query_sql_params::<types::Integer, i32, (types::Integer, types::Integer, types::Integer), _>
            ("INSERT INTO crate_owners
               (crate_id, owner_id, created_by, created_at,
                 updated_at, deleted, owner_kind)
               VALUES ($1, $2, $2, NOW(), NOW(), FALSE, $3)",
               &(self.id, user_id, OwnerKind::User as i32)));
        Ok(())
    }

    pub fn find_or_insert(conn: &GenericConnection,
                          name: &str,
                          user_id: i32,
                          description: &Option<String>,
                          homepage: &Option<String>,
                          documentation: &Option<String>,
                          readme: &Option<String>,
                          keywords: &[String],
                          repository: &Option<String>,
                          license: &Option<String>,
                          license_file: &Option<String>) -> CargoResult<Crate> {
        let description = description.as_ref().map(|s| &s[..]);
        let homepage = homepage.as_ref().map(|s| &s[..]);
        let documentation = documentation.as_ref().map(|s| &s[..]);
        let readme = readme.as_ref().map(|s| &s[..]);
        let repository = repository.as_ref().map(|s| &s[..]);
        let mut license = license.as_ref().map(|s| &s[..]);
        let license_file = license_file.as_ref().map(|s| &s[..]);
        let keywords = keywords.join(",");
        try!(validate_url(homepage, "homepage"));
        try!(validate_url(documentation, "documentation"));
        try!(validate_url(repository, "repository"));

        match license {
            // If a license is given, validate it to make sure it's actually a
            // valid license
            Some(..) => try!(validate_license(license)),

            // If no license is given, but a license file is given, flag this
            // crate as having a nonstandard license. Note that we don't
            // actually do anything else with license_file currently.
            None if license_file.is_some() => {
                license = Some("non-standard");
            }

            None => {}
        }

        // TODO: like with users, this is sadly racy
        let stmt = try!(conn.prepare("UPDATE crates
                                         SET documentation = $1,
                                             homepage = $2,
                                             description = $3,
                                             readme = $4,
                                             keywords = $5,
                                             license = $6,
                                             repository = $7
                                       WHERE canon_crate_name(name) =
                                             canon_crate_name($8)
                                   RETURNING *"));
        let rows = try!(stmt.query(&[&documentation, &homepage,
                                     &description, &readme, &keywords,
                                     &license, &repository,
                                     &name]));
        match rows.iter().next() {
            Some(row) => return Ok(Model::from_row(&row)),
            None => {}
        }

        // Blacklist the current set of crates in the rust distribution
        const RESERVED: &'static str = include_str!("reserved_crates.txt");

        if RESERVED.lines().any(|krate| name == krate) {
            return Err(human("cannot upload a crate with a reserved name"))
        }

        let stmt = try!(conn.prepare("INSERT INTO crates
                                      (name, user_id, created_at,
                                       updated_at, downloads, max_version,
                                       description, homepage, documentation,
                                       readme, keywords, repository, license)
                                      VALUES ($1, $2, $3, $3, 0, '0.0.0',
                                              $4, $5, $6, $7, $8, $9, $10)
                                      RETURNING *"));
        let now = ::now();
        let rows = try!(stmt.query(&[&name, &user_id, &now,
                                     &description, &homepage,
                                     &documentation, &readme, &keywords,
                                     &repository, &license]));
        let ret: Crate = Model::from_row(&try!(rows.iter().next().chain_error(|| {
            internal("no crate returned")
        })));

        try!(conn.execute("INSERT INTO crate_owners
                           (crate_id, owner_id, created_by, created_at,
                             updated_at, deleted, owner_kind)
                           VALUES ($1, $2, $2, $3, $3, FALSE, $4)",
                          &[&ret.id, &user_id, &now, &(OwnerKind::User as i32)]));
        return Ok(ret);

        fn validate_url(url: Option<&str>, field: &str) -> CargoResult<()> {
            let url = match url {
                Some(s) => s,
                None => return Ok(())
            };
            let url = try!(Url::parse(url).map_err(|_| {
                human(format!("`{}` is not a valid url: `{}`", field, url))
            }));
            match &url.scheme[..] {
                "http" | "https" => {}
                s => return Err(human(format!("`{}` has an invalid url \
                                               scheme: `{}`", field, s)))
            }
            match url.scheme_data {
                url::SchemeData::Relative(..) => {}
                url::SchemeData::NonRelative(..) => {
                    return Err(human(format!("`{}` must have relative scheme \
                                              data: {}", field, url)))
                }
            }
            Ok(())
        }

        fn validate_license(license: Option<&str>) -> CargoResult<()> {
            license.iter().flat_map(|s| s.split("/"))
                   .map(license_exprs::validate_license_expr)
                   .collect::<Result<Vec<_>, _>>()
                   .map(|_| ())
                   .map_err(|e| human(format!("{}; see http://opensource.org/licenses \
                                                  for options, and http://spdx.org/licenses/ \
                                                  for their identifiers", e)))
        }

    }

    pub fn valid_name(name: &str) -> bool {
        if name.len() == 0 { return false }
        name.chars().next().unwrap().is_alphabetic() &&
            name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') &&
            name.chars().all(|c| c.is_ascii())
    }

    pub fn valid_feature_name(name: &str) -> bool {
        let mut parts = name.split('/');
        match parts.next() {
            Some(part) if !Crate::valid_name(part) => return false,
            None => return false,
            _ => {}
        }
        match parts.next() {
            Some(part) if !Crate::valid_name(part) => return false,
            _ => {}
        }
        parts.next().is_none()
    }

    pub fn encodable(self, versions: Option<Vec<i32>>) -> EncodableCrate {
        let Crate {
            name, created_at, updated_at, downloads, max_version, description,
            homepage, documentation, keywords, license, repository,
            readme: _, id: _, user_id: _,
        } = self;
        let versions_link = match versions {
            Some(..) => None,
            None => Some(format!("/api/v1/crates/{}/versions", name)),
        };
        EncodableCrate {
            id: name.clone(),
            name: name.clone(),
            updated_at: ::encode_time(updated_at),
            created_at: ::encode_time(created_at),
            downloads: downloads,
            versions: versions,
            max_version: max_version.to_string(),
            documentation: documentation,
            homepage: homepage,
            description: description,
            keywords: keywords,
            license: license,
            repository: repository,
            links: CrateLinks {
                version_downloads: format!("/api/v1/crates/{}/downloads", name),
                versions: versions_link,
                owners: Some(format!("/api/v1/crates/{}/owners", name)),
                reverse_dependencies: format!("/api/v1/crates/{}/reverse_dependencies", name)
            },
        }
    }

    pub fn versions(&self, conn: &GenericConnection) -> CargoResult<Vec<Version>> {
        let stmt = try!(conn.prepare("SELECT * FROM versions \
                                      WHERE crate_id = $1"));
        let rows = try!(stmt.query(&[&self.id]));
        let mut ret = rows.iter().map(|r| {
            Model::from_row(&r)
        }).collect::<Vec<Version>>();
        ret.sort_by(|a, b| b.num.cmp(&a.num));
        Ok(ret)
    }

    pub fn owners(&self, conn: &GenericConnection) -> CargoResult<Vec<Owner>> {
        let stmt = try!(conn.prepare("SELECT * FROM users
                                      INNER JOIN crate_owners
                                         ON crate_owners.owner_id = users.id
                                      WHERE crate_owners.crate_id = $1
                                        AND crate_owners.deleted = FALSE
                                        AND crate_owners.owner_kind = $2"));
        let user_rows = try!(stmt.query(&[&self.id, &(OwnerKind::User as i32)]));

        let stmt = try!(conn.prepare("SELECT * FROM teams
                                      INNER JOIN crate_owners
                                         ON crate_owners.owner_id = teams.id
                                      WHERE crate_owners.crate_id = $1
                                        AND crate_owners.deleted = FALSE
                                        AND crate_owners.owner_kind = $2"));
        let team_rows = try!(stmt.query(&[&self.id, &(OwnerKind::Team as i32)]));

        let mut owners = vec![];
        owners.extend(user_rows.iter().map(|r| Owner::User(Model::from_row(&r))));
        owners.extend(team_rows.iter().map(|r| Owner::Team(Model::from_row(&r))));
        Ok(owners)
    }

    pub fn owner_add(&self, app: &App, conn: &GenericConnection, req_user: &User,
                     login: &str) -> CargoResult<()> {
        let owner = match Owner::find_by_login(conn, login) {
            Ok(owner @ Owner::User(_)) => { owner }
            Ok(Owner::Team(team)) => if try!(team.contains_user(app, req_user)) {
                Owner::Team(team)
            } else {
                return Err(human(format!("only members of {} can add it as \
                                          an owner", login)));
            },
            Err(err) => if login.contains(":") {
                Owner::Team(try!(Team::create(app, conn, login, req_user)))
            } else {
                return Err(err);
            },
        };

        // First try to un-delete if they've been soft deleted previously, then
        // do an insert if that didn't actually affect anything.
        let amt = try!(conn.execute("UPDATE crate_owners
                                        SET deleted = FALSE, updated_at = $1
                                      WHERE crate_id = $2 AND owner_id = $3
                                        AND owner_kind = $4",
                                    &[&::now(), &self.id, &owner.id(),
                                      &owner.kind()]));
        assert!(amt <= 1);
        if amt == 0 {
            try!(conn.execute("INSERT INTO crate_owners
                               (crate_id, owner_id, created_at, updated_at,
                                created_by, owner_kind, deleted)
                               VALUES ($1, $2, $3, $3, $4, $5, FALSE)",
                              &[&self.id, &owner.id(), &::now(), &req_user.id,
                                &owner.kind()]));
        }

        Ok(())
    }

    pub fn owner_remove(&self,
                        conn: &GenericConnection,
                        _req_user: &User,
                        login: &str) -> CargoResult<()> {
        let owner = try!(Owner::find_by_login(conn, login).map_err(|_| {
            human(format!("could not find owner with login `{}`", login))
        }));
        try!(conn.execute("UPDATE crate_owners
                              SET deleted = TRUE, updated_at = $1
                            WHERE crate_id = $2 AND owner_id = $3
                              AND owner_kind = $4",
                          &[&::now(), &self.id, &owner.id(), &owner.kind()]));
        Ok(())
    }

    pub fn s3_path(&self, version: &str) -> String {
        format!("/crates/{}/{}-{}.crate", self.name, self.name, version)
    }

    pub fn add_version(&mut self,
                       conn: &GenericConnection,
                       ver: &semver::Version,
                       features: &HashMap<String, Vec<String>>,
                       authors: &[String])
                       -> CargoResult<Version> {
        match try!(Version::find_by_num(conn, self.id, ver)) {
            Some(..) => {
                return Err(human(format!("crate version `{}` is already uploaded",
                                         ver)))
            }
            None => {}
        }
        let zero = semver::Version::parse("0.0.0").unwrap();
        if *ver > self.max_version || self.max_version == zero {
            self.max_version = ver.clone();
        }
        self.updated_at = ::now();
        try!(conn.execute("UPDATE crates SET updated_at = $1, max_version = $2
                           WHERE id = $3",
                          &[&self.updated_at, &self.max_version.to_string(),
                            &self.id]));
        Version::insert(conn, self.id, ver, features, authors)
    }

    pub fn keywords(&self, conn: &GenericConnection) -> CargoResult<Vec<Keyword>> {
        let stmt = try!(conn.prepare("SELECT keywords.* FROM keywords
                                      LEFT JOIN crates_keywords
                                      ON keywords.id = crates_keywords.keyword_id
                                      WHERE crates_keywords.crate_id = $1"));
        let rows = try!(stmt.query(&[&self.id]));
        Ok(rows.iter().map(|r| Model::from_row(&r)).collect())
    }

    /// Returns (dependency, dependent crate name)
    pub fn reverse_dependencies(&self,
                                conn: &GenericConnection,
                                offset: i64,
                                limit: i64)
                                -> CargoResult<(Vec<(Dependency, String)>, i64)> {
        let select_sql = "
              FROM dependencies
              INNER JOIN versions
                ON versions.id = dependencies.version_id
              INNER JOIN crates
                ON crates.id = versions.crate_id
              WHERE dependencies.crate_id = $1
                AND versions.num = crates.max_version
        ";
        let fetch_sql = format!("SELECT DISTINCT ON (crate_name)
                                        dependencies.*,
                                        crates.name AS crate_name
                                        {}
                               ORDER BY crate_name ASC
                                 OFFSET $2
                                  LIMIT $3", select_sql);
        let count_sql = format!("SELECT COUNT(DISTINCT(crates.id)) {}",
                                select_sql);

        let stmt = try!(conn.prepare(&fetch_sql));
        let vec: Vec<_> = try!(stmt.query(&[&self.id, &offset, &limit]))
                                   .iter().map(|r| {
            (Model::from_row(&r), r.get("crate_name"))
        }).collect();
        let stmt = try!(conn.prepare(&count_sql));
        let cnt: i64 = try!(stmt.query(&[&self.id])).iter().next().unwrap().get(0);

        Ok((vec, cnt))
    }
}

impl Model for Crate {
    fn from_row(row: &Row) -> Crate {
        let max: String = row.get("max_version");
        let kws: Option<String> = row.get("keywords");
        Crate {
            id: row.get("id"),
            name: row.get("name"),
            user_id: row.get("user_id"),
            updated_at: row.get("updated_at"),
            created_at: row.get("created_at"),
            downloads: row.get("downloads"),
            description: row.get("description"),
            documentation: row.get("documentation"),
            homepage: row.get("homepage"),
            readme: row.get("readme"),
            max_version: semver::Version::parse(&max).unwrap(),
            keywords: kws.unwrap_or(String::new()).split(',')
                         .filter(|s| !s.is_empty())
                         .map(|s| s.to_string()).collect(),
            license: row.get("license"),
            repository: row.get("repository"),
        }
    }
    fn table_name(_: Option<Crate>) -> &'static str { "crates" }
}

/// Handles the `GET /crates` route.
#[allow(trivial_casts)]
pub fn index(req: &mut Request) -> CargoResult<Response> {
    let conn = try!(req.tx());
    let (offset, limit) = try!(req.pagination(10, 100));
    let query = req.query();
    let sort = query.get("sort").map(|s| &s[..]).unwrap_or("alpha");
    let sort_sql = match sort {
        "downloads" => "ORDER BY crates.downloads DESC",
        _ => "ORDER BY crates.name ASC",
    };

    // Different queries for different parameters.
    //
    // Sure wish we had an arel-like thing here...
    let mut pattern = String::new();
    let mut id = -1;
    let (mut needs_id, mut needs_pattern) = (false, false);
    let mut args = vec![&limit as &ToSql, &offset];
    let (q, cnt) = query.get("q").map(|query| {
        args.insert(0, query);
        ("SELECT crates.* FROM crates,
                               plainto_tsquery($1) q,
                               ts_rank_cd(textsearchable_index_col, q) rank
          WHERE q @@ textsearchable_index_col
          ORDER BY rank DESC, crates.name ASC
          LIMIT $2 OFFSET $3".to_string(),
         "SELECT COUNT(crates.*) FROM crates,
                                      plainto_tsquery($1) q
          WHERE q @@ textsearchable_index_col".to_string())
    }).or_else(|| {
        query.get("letter").map(|letter| {
            pattern = format!("{}%", letter.chars().next().unwrap()
                                           .to_lowercase().collect::<String>());
            needs_pattern = true;
            (format!("SELECT * FROM crates WHERE canon_crate_name(name) \
                      LIKE $1 {} LIMIT $2 OFFSET $3", sort_sql),
             "SELECT COUNT(*) FROM crates WHERE canon_crate_name(name) \
              LIKE $1".to_string())
        })
    }).or_else(|| {
        query.get("keyword").map(|kw| {
            args.insert(0, kw);
            let base = "FROM crates
                        INNER JOIN crates_keywords
                                ON crates.id = crates_keywords.crate_id
                        INNER JOIN keywords
                                ON crates_keywords.keyword_id = keywords.id
                        WHERE lower(keywords.keyword) = lower($1)";
            (format!("SELECT crates.* {} {} LIMIT $2 OFFSET $3", base, sort_sql),
             format!("SELECT COUNT(crates.*) {}", base))
        })
    }).or_else(|| {
        query.get("user_id").and_then(|s| s.parse::<i32>().ok()).map(|user_id| {
            id = user_id;
            needs_id = true;
            (format!("SELECT crates.* FROM crates
                       INNER JOIN crate_owners
                          ON crate_owners.crate_id = crates.id
                       WHERE crate_owners.owner_id = $1
                       AND crate_owners.owner_kind = {} {}
                      LIMIT $2 OFFSET $3",
                     OwnerKind::User as i32, sort_sql),
             format!("SELECT COUNT(crates.*) FROM crates
               INNER JOIN crate_owners
                  ON crate_owners.crate_id = crates.id
               WHERE crate_owners.owner_id = $1 \
                 AND crate_owners.owner_kind = {}",
                 OwnerKind::User as i32))
        })
    }).or_else(|| {
        query.get("following").map(|_| {
            needs_id = true;
            (format!("SELECT crates.* FROM crates
                      INNER JOIN follows
                         ON follows.crate_id = crates.id AND
                            follows.user_id = $1
                      {} LIMIT $2 OFFSET $3", sort_sql),
             "SELECT COUNT(crates.*) FROM crates
              INNER JOIN follows
                 ON follows.crate_id = crates.id AND
                    follows.user_id = $1".to_string())
        })
    }).unwrap_or_else(|| {
        (format!("SELECT * FROM crates {} LIMIT $1 OFFSET $2",
                 sort_sql),
         "SELECT COUNT(*) FROM crates".to_string())
    });

    if needs_id {
        if id == -1 {
            id = try!(req.user()).id;
        }
        args.insert(0, &id);
    } else if needs_pattern {
        args.insert(0, &pattern);
    }

    // Collect all the crates
    let stmt = try!(conn.prepare(&q));
    let mut crates = Vec::new();
    for row in try!(stmt.query(&args)) {
        let krate: Crate = Model::from_row(&row);
        crates.push(krate.encodable(None));
    }

    // Query for the total count of crates
    let stmt = try!(conn.prepare(&cnt));
    let args = if args.len() > 2 {&args[..1]} else {&args[..0]};
    let row = try!(stmt.query(args)).into_iter().next().unwrap();
    let total = row.get(0);

    #[derive(RustcEncodable)]
    struct R { crates: Vec<EncodableCrate>, meta: Meta }
    #[derive(RustcEncodable)]
    struct Meta { total: i64 }

    Ok(req.json(&R {
        crates: crates,
        meta: Meta { total: total },
    }))
}

/// Handles the `GET /summary` route.
pub fn summary(req: &mut Request) -> CargoResult<Response> {
    use self::crates::dsl::*;
    use self::metadata::dsl::*;

    let conn = req.new_conn();
    let num_crates = try!(conn.query_one(crates.count())).unwrap();
    let num_downloads = try!(conn.query_one(metadata.select(total_downloads))).unwrap();

    let new_crates: Vec<_> = try!(crates.order(created_at.desc()).limit(10)
        .load(conn))
        .map(|c: Crate| c.encodable(None))
        .collect();
    let just_updated: Vec<_> = try!(crates.filter(updated_at.ne(created_at))
        .order(updated_at.desc())
        .limit(10)
        .load(conn))
        .map(|c: Crate| c.encodable(None))
        .collect();
    let most_downloaded: Vec<_> = try!(crates.order(downloads.desc()).limit(10).load(conn))
        .map(|c: Crate| c.encodable(None))
        .collect();

    #[derive(RustcEncodable)]
    struct R {
        num_downloads: i64,
        num_crates: i64,
        new_crates: Vec<EncodableCrate>,
        most_downloaded: Vec<EncodableCrate>,
        just_updated: Vec<EncodableCrate>,
    }
    Ok(req.json(&R {
        num_downloads: num_downloads,
        num_crates: num_crates,
        new_crates: new_crates,
        most_downloaded: most_downloaded,
        just_updated: just_updated,
    }))
}

/// Handles the `GET /crates/:crate_id` route.
pub fn show(req: &mut Request) -> CargoResult<Response> {
    let name = &req.params()["crate_id"];
    let conn = try!(req.tx());
    let krate = try!(Crate::find_by_name(conn, &name));
    let versions = try!(krate.versions(conn));
    let ids = versions.iter().map(|v| v.id).collect();
    let kws = try!(krate.keywords(conn));

    #[derive(RustcEncodable)]
    struct R {
        krate: EncodableCrate,
        versions: Vec<EncodableVersion>,
        keywords: Vec<EncodableKeyword>,
    }
    Ok(req.json(&R {
        krate: krate.clone().encodable(Some(ids)),
        versions: versions.into_iter().map(|v| {
            v.encodable(&krate.name)
        }).collect(),
        keywords: kws.into_iter().map(|k| k.encodable()).collect(),
    }))
}

/// Handles the `PUT /crates/new` route.
pub fn new(req: &mut Request) -> CargoResult<Response> {
    let app = req.app().clone();

    let (new_crate, user) = try!(parse_new_headers(req));
    let name = &*new_crate.name;
    let vers = &*new_crate.vers;
    let features = new_crate.features.iter().map(|(k, v)| {
        (k[..].to_string(), v.iter().map(|v| v[..].to_string()).collect())
    }).collect::<HashMap<String, Vec<String>>>();
    let keywords = new_crate.keywords.as_ref().map(|s| &s[..])
                                     .unwrap_or(&[]);
    let keywords = keywords.iter().map(|k| k[..].to_string()).collect::<Vec<_>>();

    // Persist the new crate, if it doesn't already exist
    let mut krate = try!(Crate::find_or_insert(try!(req.tx()), name, user.id,
                                               &new_crate.description,
                                               &new_crate.homepage,
                                               &new_crate.documentation,
                                               &new_crate.readme,
                                               &keywords,
                                               &new_crate.repository,
                                               &new_crate.license,
                                               &new_crate.license_file));

    let owners = try!(krate.owners(try!(req.tx())));
    if try!(rights(req.app(), &owners, &user)) < Rights::Publish {
        return Err(human("crate name has already been claimed by \
                          another user"))
    }

    if krate.name != name {
        return Err(human(format!("crate was previously named `{}`", krate.name)))
    }

    // Persist the new version of this crate
    let mut version = try!(krate.add_version(try!(req.tx()), vers, &features,
                                             &new_crate.authors));

    // Link this new version to all dependencies
    let mut deps = Vec::new();
    for dep in new_crate.deps.iter() {
        let (dep, krate) = try!(version.add_dependency(try!(req.tx()), dep));
        deps.push(dep.git_encode(&krate.name));
    }

    // Update all keywords for this crate
    try!(Keyword::update_crate(try!(req.tx()), &krate, &keywords));

    // Upload the crate to S3
    let mut handle = req.app().handle();
    let path = krate.s3_path(&vers.to_string());
    let (resp, cksum) = {
        let length = try!(read_le_u32(req.body()));
        let body = LimitErrorReader::new(req.body(), app.config.max_upload_size);
        let mut body = HashingReader::new(body);
        let resp = {
            let s3req = app.bucket.put(&mut handle, &path, &mut body,
                                       "application/x-tar")
                                  .content_length(length as usize);
            try!(s3req.exec().chain_error(|| {
                internal(format!("failed to upload to S3: `{}`", path))
            }))
        };
        (resp, body.finalize())
    };
    if resp.get_code() != 200 {
        return Err(internal(format!("failed to get a 200 response from S3: {}",
                                    resp)))
    }

    // If the git commands fail below, we shouldn't keep the crate on the
    // server.
    struct Bomb { app: Arc<App>, path: Option<String>, handle: http::Handle }
    impl Drop for Bomb {
        fn drop(&mut self) {
            match self.path {
                Some(ref path) => {
                    let _ = self.app.bucket.delete(&mut self.handle, &path)
                                .exec();
                }
                None => {}
            }
        }
    }
    let mut bomb = Bomb { app: app.clone(), path: Some(path), handle: handle };

    // Register this crate in our local git repo.
    let git_crate = git::Crate {
        name: name.to_string(),
        vers: vers.to_string(),
        cksum: cksum.to_hex(),
        features: features,
        deps: deps,
        yanked: Some(false),
    };
    try!(git::add_crate(&**req.app(), &git_crate).chain_error(|| {
        internal(format!("could not add crate `{}` to the git repo", name))
    }));

    // Now that we've come this far, we're committed!
    bomb.path = None;

    #[derive(RustcEncodable)]
    struct R { krate: EncodableCrate }
    Ok(req.json(&R { krate: krate.encodable(None) }))
}

fn parse_new_headers(req: &mut Request) -> CargoResult<(upload::NewCrate, User)> {
    // Make sure the tarball being uploaded looks sane
    let length = try!(req.content_length().chain_error(|| {
        human("missing header: Content-Length")
    }));
    let max = req.app().config.max_upload_size;
    if length > max {
        return Err(human(format!("max upload size is: {}", max)))
    }

    // Read the json upload request
    let amt = try!(read_le_u32(req.body())) as u64;
    if amt > max { return Err(human(format!("max upload size is: {}", max))) }
    let mut json = repeat(0).take(amt as usize).collect::<Vec<_>>();
    try!(read_fill(req.body(), &mut json));
    let json = try!(String::from_utf8(json).map_err(|_| {
        human("json body was not valid utf-8")
    }));
    let new: upload::NewCrate = try!(json::decode(&json).map_err(|e| {
        human(format!("invalid upload request: {:?}", e))
    }));

    // Make sure required fields are provided
    fn empty(s: Option<&String>) -> bool { s.map_or(true, |s| s.is_empty()) }
    let mut missing = Vec::new();

    if empty(new.description.as_ref()) {
        missing.push("description");
    }
    if empty(new.license.as_ref()) && empty(new.license_file.as_ref()) {
        missing.push("license");
    }
    if new.authors.len() == 0 || new.authors.iter().all(|s| s.is_empty()) {
        missing.push("authors");
    }
    if missing.len() > 0 {
        return Err(human(format!("missing or empty metadata fields: {}. Please \
            see http://doc.crates.io/manifest.html#package-metadata for \
            how to upload metadata", missing.join(", "))));
    }

    let user = try!(req.user());
    Ok((new, user.clone()))
}

fn read_le_u32<R: Read + ?Sized>(r: &mut R) -> io::Result<u32> {
    let mut b = [0; 4];
    try!(read_fill(r, &mut b));
    Ok(((b[0] as u32) <<  0) |
       ((b[1] as u32) <<  8) |
       ((b[2] as u32) << 16) |
       ((b[3] as u32) << 24))
}

fn read_fill<R: Read + ?Sized>(r: &mut R, mut slice: &mut [u8])
                               -> io::Result<()> {
    while slice.len() > 0 {
        let n = try!(r.read(slice));
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::Other,
                                      "end of file reached"))
        }
        slice = &mut mem::replace(&mut slice, &mut [])[n..];
    }
    Ok(())
}

sql_function!(canon_crate_name, (a: VarChar) -> VarChar);

/// Handles the `GET /crates/:crate_id/:version/download` route.
pub fn download(req: &mut Request) -> CargoResult<Response> {
    use yaqb::expression::dsl::*;
    use self::version_downloads::downloads;

    let crate_name = &req.params()["crate_id"];
    let version = &req.params()["version"];
    let conn = req.new_conn();

    let version_id = try!(crates::table.inner_join(versions::table)
        .select(versions::id)
        .filter(canon_crate_name(crates::name).eq(canon_crate_name(crate_name)))
        .filter(versions::num.eq(version))
        .first(&conn)
        .map_err(|e| e.into())
        .and_then(|r| r.chain_error(|| human("crate or version not found"))));

    // Bump download counts.
    //
    // Note that this is *not* an atomic update, and that's somewhat
    // intentional. It doesn't appear that postgres supports an atomic update of
    // a counter, so we just do the hopefully "least racy" thing. This is
    // largely ok because these download counters are just that, counters. No
    // need to have super high-fidelity counter.
    //
    // Also, we only update the counter for *today*, nothing else. We have lots
    // of other counters, but they're all updated later on via the
    // update-downloads script.
    let target = version_downloads::table
        .filter(version_downloads::version_id.eq(version_id))
        .filter(date(now).eq(date(version_downloads::date)));
    let command = update(target).set(downloads.eq(downloads + 1));
    let updated_rows = try!(conn.execute_returning_count(&command));

    if updated_rows == 0 {
        let new_download = NewVersionDownload::new(version_id);
        try!(conn.insert_returning_count(&version_downloads::table, &[new_download]));
    }

    // Now that we've done our business, redirect to the actual data.
    let redirect_url = format!("https://{}/crates/{}/{}-{}.crate",
                               req.app().bucket.host(),
                               crate_name, crate_name, version);

    if req.wants_json() {
        #[derive(RustcEncodable)]
        struct R { url: String }
        Ok(req.json(&R{ url: redirect_url }))
    } else {
        Ok(req.redirect(redirect_url))
    }
}

/// Handles the `GET /crates/:crate_id/downloads` route.
pub fn downloads(req: &mut Request) -> CargoResult<Response> {
    let crate_name = &req.params()["crate_id"];
    let tx = try!(req.tx());
    let krate = try!(Crate::find_by_name(tx, crate_name));
    let mut versions = try!(krate.versions(tx));
    versions.sort_by(|a, b| b.num.cmp(&a.num));


    let to_show = &versions[..cmp::min(5, versions.len())];
    let ids = to_show.iter().map(|i| i.id).collect::<Vec<_>>();

    let cutoff_date = ::now() + Duration::days(-90);
    let stmt = try!(tx.prepare("SELECT * FROM version_downloads
                                 WHERE date > $1
                                   AND version_id = ANY($2)
                                 ORDER BY date ASC"));
    let mut downloads = Vec::new();
    for row in try!(stmt.query(&[&cutoff_date, &Slice(&ids)])) {
        let download: VersionDownload = Model::from_row(&row);
        downloads.push(download.encodable());
    }

    let stmt = try!(tx.prepare("\
          SELECT COALESCE(to_char(DATE(version_downloads.date), 'YYYY-MM-DD'), '') AS date,
                 SUM(version_downloads.downloads) AS downloads
            FROM version_downloads
           INNER JOIN versions ON
                 version_id = versions.id
           WHERE version_downloads.date > $1
             AND versions.crate_id = $2
             AND NOT (versions.id = ANY($3))
        GROUP BY DATE(version_downloads.date)
        ORDER BY DATE(version_downloads.date) ASC"));
    let mut extra = Vec::new();
    for row in try!(stmt.query(&[&cutoff_date, &krate.id, &Slice(&ids)])) {
        extra.push(ExtraDownload {
            downloads: row.get("downloads"),
            date: row.get("date")
        });
    }

    #[derive(RustcEncodable)]
    struct ExtraDownload { date: String, downloads: i64 }
    #[derive(RustcEncodable)]
    struct R { version_downloads: Vec<EncodableVersionDownload>, meta: Meta }
    #[derive(RustcEncodable)]
    struct Meta { extra_downloads: Vec<ExtraDownload> }
    let meta = Meta { extra_downloads: extra };
    Ok(req.json(&R{ version_downloads: downloads, meta: meta }))
}

fn user_and_crate(req: &mut Request) -> CargoResult<(User, Crate)> {
    let user = try!(req.user());
    let crate_name = &req.params()["crate_id"];
    let tx = try!(req.tx());
    let krate = try!(Crate::find_by_name(tx, crate_name));
    Ok((user.clone(), krate))
}

/// Handles the `PUT /crates/:crate_id/follow` route.
pub fn follow(req: &mut Request) -> CargoResult<Response> {
    let (user, krate) = try!(user_and_crate(req));
    let tx = try!(req.tx());
    let stmt = try!(tx.prepare("SELECT 1 FROM follows
                                WHERE user_id = $1 AND crate_id = $2"));
    let rows = try!(stmt.query(&[&user.id, &krate.id]));
    if !rows.iter().next().is_some() {
        try!(tx.execute("INSERT INTO follows (user_id, crate_id)
                         VALUES ($1, $2)", &[&user.id, &krate.id]));
    }
    #[derive(RustcEncodable)]
    struct R { ok: bool }
    Ok(req.json(&R { ok: true }))
}

/// Handles the `DELETE /crates/:crate_id/follow` route.
pub fn unfollow(req: &mut Request) -> CargoResult<Response> {
    let (user, krate) = try!(user_and_crate(req));
    let tx = try!(req.tx());
    try!(tx.execute("DELETE FROM follows
                     WHERE user_id = $1 AND crate_id = $2",
                    &[&user.id, &krate.id]));
    #[derive(RustcEncodable)]
    struct R { ok: bool }
    Ok(req.json(&R { ok: true }))
}

/// Handles the `GET /crates/:crate_id/following` route.
pub fn following(req: &mut Request) -> CargoResult<Response> {
    let (user, krate) = try!(user_and_crate(req));
    let tx = try!(req.tx());
    let stmt = try!(tx.prepare("SELECT 1 FROM follows
                                WHERE user_id = $1 AND crate_id = $2"));
    let mut rows = try!(stmt.query(&[&user.id, &krate.id])).into_iter();
    #[derive(RustcEncodable)]
    struct R { following: bool }
    Ok(req.json(&R { following: rows.next().is_some() }))
}

/// Handles the `GET /crates/:crate_id/versions` route.
pub fn versions(req: &mut Request) -> CargoResult<Response> {
    let crate_name = &req.params()["crate_id"];
    let tx = try!(req.tx());
    let krate = try!(Crate::find_by_name(tx, crate_name));
    let versions = try!(krate.versions(tx));
    let versions = versions.into_iter().map(|v| v.encodable(crate_name))
                           .collect();

    #[derive(RustcEncodable)]
    struct R { versions: Vec<EncodableVersion> }
    Ok(req.json(&R{ versions: versions }))
}

/// Handles the `GET /crates/:crate_id/owners` route.
pub fn owners(req: &mut Request) -> CargoResult<Response> {
    let crate_name = &req.params()["crate_id"];
    let tx = try!(req.tx());
    let krate = try!(Crate::find_by_name(tx, crate_name));
    let owners = try!(krate.owners(tx));
    let owners = owners.into_iter().map(|o| o.encodable()).collect();

    #[derive(RustcEncodable)]
    struct R { users: Vec<EncodableOwner> }
    Ok(req.json(&R{ users: owners }))
}

/// Handles the `PUT /crates/:crate_id/owners` route.
pub fn add_owners(req: &mut Request) -> CargoResult<Response> {
    modify_owners(req, true)
}

/// Handles the `DELETE /crates/:crate_id/owners` route.
pub fn remove_owners(req: &mut Request) -> CargoResult<Response> {
    modify_owners(req, false)
}

fn modify_owners(req: &mut Request, add: bool) -> CargoResult<Response> {
    let mut body = String::new();
    try!(req.body().read_to_string(&mut body));
    let (user, krate) = try!(user_and_crate(req));
    let tx = try!(req.tx());
    let owners = try!(krate.owners(tx));

    match try!(rights(req.app(), &owners, &user)) {
        Rights::Full => {} // Yes!
        Rights::Publish => {
            return Err(human("team members don't have permission to modify owners"));
        }
        Rights::None => {
            return Err(human("only owners have permission to modify owners"));
        }
    }

    #[derive(RustcDecodable)]
    struct Request {
        // identical, for back-compat (owners preferred)
        users: Option<Vec<String>>,
        owners: Option<Vec<String>>,
    }

    let request: Request = try!(json::decode(&body).map_err(|_| {
        human("invalid json request")
    }));

    let logins = try!(request.owners.or(request.users).ok_or_else(|| {
        human("invalid json request")
    }));

    for login in &logins {
        if add {
            if owners.iter().any(|owner| owner.login() == *login) {
                return Err(human(format!("`{}` is already an owner", login)))
            }
            try!(krate.owner_add(req.app(), tx, &user, &login));
        } else {
            // Removing the team that gives you rights is prevented because
            // team members only have Rights::Publish
            if *login == user.gh_login {
                return Err(human("cannot remove yourself as an owner"))
            }
            try!(krate.owner_remove(tx, &user, &login));
        }
    }

    #[derive(RustcEncodable)]
    struct R { ok: bool }
    Ok(req.json(&R{ ok: true }))
}

/// Handles the `GET /crates/:crate_id/reverse_dependencies` route.
pub fn reverse_dependencies(req: &mut Request) -> CargoResult<Response> {
    let name = &req.params()["crate_id"];
    let conn = try!(req.tx());
    let krate = try!(Crate::find_by_name(conn, &name));
    let tx = try!(req.tx());
    let (offset, limit) = try!(req.pagination(10, 100));
    let (rev_deps, total) = try!(krate.reverse_dependencies(tx, offset, limit));
    let rev_deps = rev_deps.into_iter().map(|(dep, crate_name)| {
        dep.encodable(&crate_name)
    }).collect();

    #[derive(RustcEncodable)]
    struct R { dependencies: Vec<EncodableDependency>, meta: Meta }
    #[derive(RustcEncodable)]
    struct Meta { total: i64 }
    Ok(req.json(&R{ dependencies: rev_deps, meta: Meta { total: total } }))
}
