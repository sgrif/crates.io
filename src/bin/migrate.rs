#![deny(warnings)]
#![feature(iter_cmp)]

extern crate cargo_registry;
extern crate migrate;
extern crate postgres;

use std::env;
use std::collections::HashSet;
use migrate::Migration;

use cargo_registry::krate::Crate;
use cargo_registry::model::Model;

fn main() {
    let conn = postgres::Connection::connect(&env("DATABASE_URL")[..],
                                             &postgres::SslMode::None).unwrap();
    let migrations = migrations();

    let arg = env::args().nth(1);
    if arg.as_ref().map(|s| &s[..]) == Some("rollback") {
        rollback(conn.transaction().unwrap(), migrations).unwrap();
    } else {
        apply(conn.transaction().unwrap(), migrations).unwrap();
    }

    fn env(s: &str) -> String {
        match env::var(s).ok() {
            Some(s) => s,
            None => panic!("must have `{}` defined", s),
        }
    }
}

fn apply(tx: postgres::Transaction,
         migrations: Vec<Migration>) -> postgres::Result<()> {
    let mut mgr = try!(migrate::Manager::new(tx));
    for m in migrations.into_iter() {
        try!(mgr.apply(m));
    }
    mgr.set_commit();
    mgr.finish()
}

fn rollback(tx: postgres::Transaction,
            migrations: Vec<Migration>) -> postgres::Result<()> {
    let mut mgr = try!(migrate::Manager::new(tx));
    for m in migrations.into_iter().rev() {
        if mgr.contains(m.version()) {
            try!(mgr.rollback(m));
            break
        }
    }
    mgr.set_commit();
    mgr.finish()
}

fn migrations() -> Vec<Migration> {
    let migrations = vec![
        Migration::add_table(20140924113530, "users", "
            id              SERIAL PRIMARY KEY,
            email           VARCHAR NOT NULL UNIQUE,
            gh_access_token VARCHAR NOT NULL,
            api_token       VARCHAR NOT NULL
        "),
        Migration::add_table(20140924114003, "packages", "
            id              SERIAL PRIMARY KEY,
            name            VARCHAR NOT NULL UNIQUE,
            user_id         INTEGER NOT NULL
        "),
        Migration::add_table(20140924114059, "versions", "
            id              SERIAL PRIMARY KEY,
            package_id      INTEGER NOT NULL,
            num             VARCHAR NOT NULL
        "),
        Migration::run(20140924115329,
                       &format!("ALTER TABLE versions ADD CONSTRAINT \
                                 unique_num UNIQUE (package_id, num)"),
                       &format!("ALTER TABLE versions DROP CONSTRAINT \
                                 unique_num")),
        Migration::add_table(20140924120803, "version_dependencies", "
            version_id      INTEGER NOT NULL,
            depends_on_id   INTEGER NOT NULL
        "),
        Migration::add_column(20140925132248, "packages", "updated_at",
                              "TIMESTAMP NOT NULL DEFAULT now()"),
        Migration::add_column(20140925132249, "packages", "created_at",
                              "TIMESTAMP NOT NULL DEFAULT now()"),
        Migration::new(20140925132250, |tx| {
            try!(tx.execute("UPDATE packages SET updated_at = now() \
                             WHERE updated_at IS NULL", &[]));
            try!(tx.execute("UPDATE packages SET created_at = now() \
                             WHERE created_at IS NULL", &[]));
            Ok(())
        }, |_| Ok(())),
        Migration::add_column(20140925132251, "versions", "updated_at",
                              "TIMESTAMP NOT NULL DEFAULT now()"),
        Migration::add_column(20140925132252, "versions", "created_at",
                              "TIMESTAMP NOT NULL DEFAULT now()"),
        Migration::new(20140925132253, |tx| {
            try!(tx.execute("UPDATE versions SET updated_at = now() \
                             WHERE updated_at IS NULL", &[]));
            try!(tx.execute("UPDATE versions SET created_at = now() \
                             WHERE created_at IS NULL", &[]));
            Ok(())
        }, |_| Ok(())),
        Migration::new(20140925132254, |tx| {
            try!(tx.execute("ALTER TABLE versions ALTER COLUMN updated_at \
                             DROP DEFAULT", &[]));
            try!(tx.execute("ALTER TABLE versions ALTER COLUMN created_at \
                             DROP DEFAULT", &[]));
            try!(tx.execute("ALTER TABLE packages ALTER COLUMN updated_at \
                             DROP DEFAULT", &[]));
            try!(tx.execute("ALTER TABLE packages ALTER COLUMN created_at \
                             DROP DEFAULT", &[]));
            Ok(())
        }, |_| Ok(())),
        Migration::add_table(20140925153704, "metadata", "
            total_downloads        BIGINT NOT NULL
        "),
        Migration::new(20140925153705, |tx| {
            try!(tx.execute("INSERT INTO metadata (total_downloads) \
                             VALUES ($1)", &[&0i64]));
            Ok(())
        }, |tx| {
            try!(tx.execute("DELETE FROM metadata", &[])); Ok(())
        }),
        Migration::add_column(20140925161623, "packages", "downloads",
                              "INTEGER NOT NULL DEFAULT 0"),
        Migration::add_column(20140925161624, "versions", "downloads",
                              "INTEGER NOT NULL DEFAULT 0"),
        Migration::new(20140925161625, |tx| {
            try!(tx.execute("ALTER TABLE versions ALTER COLUMN downloads \
                             DROP DEFAULT", &[]));
            try!(tx.execute("ALTER TABLE packages ALTER COLUMN downloads \
                             DROP DEFAULT", &[]));
            Ok(())
        }, |_| Ok(())),
        Migration::add_column(20140926130044, "packages", "max_version",
                              "VARCHAR"),
        Migration::new(20140926130045, |tx| {
            let stmt = try!(tx.prepare("SELECT * FROM packages"));
            for row in try!(stmt.query(&[])) {
                let pkg: Crate = Model::from_row(&row);
                let versions = pkg.versions(tx).unwrap();
                let v = versions.iter().max_by(|v| &v.num).unwrap();
                let max = v.num.to_string();
                try!(tx.execute("UPDATE packages SET max_version = $1 \
                                 WHERE id = $2",
                                &[&max, &pkg.id]));
            }
            Ok(())
        }, |_| Ok(())),
        Migration::new(20140926130046, |tx| {
            try!(tx.execute("ALTER TABLE versions ALTER COLUMN downloads \
                             SET NOT NULL", &[]));
            Ok(())
        }, |tx| {
            try!(tx.execute("ALTER TABLE versions ALTER COLUMN downloads \
                             DROP NOT NULL", &[]));
            Ok(())
        }),
        Migration::new(20140926174020, |tx| {
            try!(tx.execute("ALTER TABLE packages RENAME TO crates", &[]));
            try!(tx.execute("ALTER TABLE versions RENAME COLUMN package_id \
                             TO crate_id", &[]));
            Ok(())
        }, |tx| {
            try!(tx.execute("ALTER TABLE crates RENAME TO packages", &[]));
            try!(tx.execute("ALTER TABLE versions RENAME COLUMN crate_id \
                             TO package_id", &[]));
            Ok(())
        }),
        Migration::run(20140929103749,
                       "CREATE INDEX index_crate_updated_at ON crates (updated_at)",
                       "DROP INDEX index_crate_updated_at"),
        Migration::run(20140929103750,
                       "CREATE INDEX index_crate_created_at ON crates (created_at)",
                       "DROP INDEX index_crate_created_at"),
        Migration::run(20140929103751,
                       "CREATE INDEX index_crate_downloads ON crates (downloads)",
                       "DROP INDEX index_crate_downloads"),
        Migration::run(20140929103752,
                       "CREATE INDEX index_version_crate_id ON versions (crate_id)",
                       "DROP INDEX index_version_crate_id"),
        Migration::run(20140929103753,
                       "CREATE INDEX index_version_num ON versions (num)",
                       "DROP INDEX index_version_num"),
        Migration::run(20140929103754,
                       "CREATE INDEX index_version_dependencies_version_id \
                        ON version_dependencies (version_id)",
                       "DROP INDEX index_version_dependencies_version_id"),
        Migration::run(20140929103755,
                       "CREATE INDEX index_version_dependencies_depends_on_id \
                        ON version_dependencies (depends_on_id)",
                       "DROP INDEX index_version_dependencies_depends_on_id"),
        Migration::add_table(20140929103756, "crate_downloads", "
            id              SERIAL PRIMARY KEY,
            crate_id        INTEGER NOT NULL,
            downloads       INTEGER NOT NULL,
            date            TIMESTAMP NOT NULL
        "),
        Migration::run(20140929103757,
                       "CREATE INDEX index_crate_downloads_crate_id \
                        ON crate_downloads (crate_id)",
                       "DROP INDEX index_crate_downloads_crate_id"),
        Migration::run(20140929103758,
                       "CREATE INDEX index_crate_downloads_date \
                        ON crate_downloads (date(date))",
                       "DROP INDEX index_crate_downloads_date"),
        Migration::add_table(20140929103759, "version_downloads", "
            id              SERIAL PRIMARY KEY,
            version_id      INTEGER NOT NULL,
            downloads       INTEGER NOT NULL,
            counted         INTEGER NOT NULL,
            date            TIMESTAMP NOT NULL,
            processed       BOOLEAN NOT NULL
        "),
        Migration::run(20140929103760,
                       "CREATE INDEX index_version_downloads_version_id \
                        ON version_downloads (version_id)",
                       "DROP INDEX index_version_downloads_version_id"),
        Migration::run(20140929103761,
                       "CREATE INDEX index_version_downloads_date \
                        ON version_downloads (date(date))",
                       "DROP INDEX index_version_downloads_date"),
        Migration::run(20140929103763,
                       "CREATE INDEX index_version_downloads_processed \
                        ON version_downloads (processed)
                        WHERE processed = FALSE",
                       "DROP INDEX index_version_downloads_processed"),
        Migration::run(20140929185718,
                       "CREATE INDEX index_crates_name_search \
                        ON crates USING gin(to_tsvector('english', name))",
                       "DROP INDEX index_crates_name_search"),
        Migration::run(20140930082104,
                       "DROP TABLE version_dependencies",
                       "CREATE TABLE version_dependencies (
                            version_id INTEGER
                        )"),
        Migration::add_table(20140930082105, "dependencies", "
            id               SERIAL PRIMARY KEY,
            version_id       INTEGER NOT NULL,
            crate_id         INTEGER NOT NULL,
            req              VARCHAR NOT NULL,
            optional         BOOLEAN NOT NULL,
            default_features BOOLEAN NOT NULL,
            features         VARCHAR NOT NULL
        "),
        Migration::add_column(20140930085441, "versions", "features",
                              "VARCHAR"),
        Migration::run(20140930203145,
                       "CREATE INDEX index_dependencies_version_id \
                        ON dependencies (version_id)",
                       "DROP INDEX index_dependencies_version_id"),
        Migration::run(20140930203146,
                       "CREATE INDEX index_dependencies_crate_id \
                        ON dependencies (crate_id)",
                       "DROP INDEX index_dependencies_crate_id"),
        Migration::add_column(20141001190227, "users", "gh_login",
                              "VARCHAR NOT NULL"),
        Migration::add_column(20141001190228, "users", "name", "VARCHAR"),
        Migration::run(20141001190229,
                       "CREATE INDEX index_users_gh_login \
                        ON users (gh_login)",
                       "DROP INDEX index_users_gh_login"),
        Migration::run(20141001190230,
                       "ALTER TABLE users ALTER COLUMN email DROP NOT NULL",
                       "ALTER TABLE users ALTER COLUMN email SET NOT NULL"),
        Migration::add_column(20141001190231, "users", "gh_avatar", "VARCHAR"),
        Migration::run(20141002195939,
                       "CREATE INDEX index_crates_user_id \
                        ON crates (user_id)",
                       "DROP INDEX index_crates_user_id"),
        Migration::add_table(20141002195940, "follows", "
            user_id          INTEGER NOT NULL,
            crate_id         INTEGER NOT NULL
        "),
        Migration::run(20141002195941,
                       "CREATE INDEX index_follows_user_id \
                        ON follows (user_id)",
                       "DROP INDEX index_follows_user_id"),
        foreign_key(20141002222426, "crate_downloads", "crate_id", "crates (id)"),
        foreign_key(20141002222427, "crates", "user_id", "users (id)"),
        foreign_key(20141002222428, "dependencies", "version_id", "versions (id)"),
        foreign_key(20141002222429, "dependencies", "crate_id", "crates (id)"),
        foreign_key(20141002222430, "follows", "crate_id", "crates (id)"),
        foreign_key(20141002222431, "version_downloads", "version_id",
                    "versions (id)"),
        foreign_key(20141002222432, "versions", "crate_id", "crates (id)"),
        foreign_key(20141002222433, "follows", "user_id", "users (id)"),
        Migration::add_table(20141007131146, "version_authors", "
            id               SERIAL PRIMARY KEY,
            version_id       INTEGER NOT NULL,
            user_id          INTEGER,
            name             VARCHAR NOT NULL
        "),
        foreign_key(20141007131147, "version_authors", "user_id", "users (id)"),
        foreign_key(20141007131148, "version_authors", "version_id", "versions (id)"),
        index(20141007131149, "version_authors", "version_id"),

        Migration::add_table(20141007131735, "crate_owners", "
            id               SERIAL PRIMARY KEY,
            crate_id         INTEGER NOT NULL,
            user_id          INTEGER NOT NULL,
            created_at       TIMESTAMP NOT NULL,
            created_by       INTEGER
        "),
        foreign_key(20141007131736, "crate_owners", "user_id", "users (id)"),
        foreign_key(20141007131737, "crate_owners", "created_by", "users (id)"),
        foreign_key(20141007131738, "crate_owners", "crate_id", "crates (id)"),
        index(20141007131739, "crate_owners", "crate_id"),
        Migration::add_column(20141007131740, "crate_owners", "deleted",
                              "BOOLEAN NOT NULL"),
        Migration::add_column(20141007131741, "crate_owners", "updated_at",
                              "TIMESTAMP NOT NULL"),
        Migration::add_column(20141007171515, "crates", "description",
                              "VARCHAR"),
        Migration::add_column(20141007171516, "crates", "homepage",
                              "VARCHAR"),
        Migration::add_column(20141007171517, "crates", "documentation",
                              "VARCHAR"),
        Migration::add_column(20141010150327, "crates", "readme", "VARCHAR"),
        Migration::add_column(20141013115510, "versions", "yanked",
                              "BOOLEAN DEFAULT FALSE"),
        Migration::add_column(20141020175647, "crates",
                              "textsearchable_index_col", "tsvector"),
        Migration::run(20141020175648,
                       "DROP INDEX index_crates_name_search",
                       "CREATE INDEX index_crates_name_search \
                        ON crates USING gin(to_tsvector('english', name))"),
        Migration::run(20141020175649,
                       "CREATE INDEX index_crates_name_search \
                        ON crates USING gin(textsearchable_index_col)",
                       "DROP INDEX index_crates_name_search"),

        // http://www.postgresql.org/docs/8.3/static/textsearch-controls.html
        // http://www.postgresql.org/docs/8.3/static/textsearch-features.html
        Migration::new(20141020175650, |tx| {
            try!(tx.batch_execute("
            CREATE FUNCTION trigger_crates_name_search() RETURNS trigger AS $$
            begin
              new.textsearchable_index_col :=
                 setweight(to_tsvector('pg_catalog.english',
                                       coalesce(new.name, '')), 'A') ||
                 setweight(to_tsvector('pg_catalog.english',
                                       coalesce(new.keywords, '')), 'B') ||
                 setweight(to_tsvector('pg_catalog.english',
                                       coalesce(new.description, '')), 'C') ||
                 setweight(to_tsvector('pg_catalog.english',
                                       coalesce(new.readme, '')), 'D');
              return new;
            end
            $$ LANGUAGE plpgsql;

            CREATE TRIGGER trigger_crates_tsvector_update BEFORE INSERT OR UPDATE
            ON crates
            FOR EACH ROW EXECUTE PROCEDURE trigger_crates_name_search();
            "));
            Ok(())

        }, |tx| {
            try!(tx.execute("DROP TRIGGER trigger_crates_tsvector_update
                                       ON crates", &[]));
            try!(tx.execute("DROP FUNCTION trigger_crates_name_search()", &[]));
            Ok(())
        }),
        Migration::add_column(20141020175651, "crates", "keywords", "varchar"),
        Migration::add_table(20141021103503, "keywords", "
            id               SERIAL PRIMARY KEY,
            keyword          VARCHAR NOT NULL UNIQUE,
            crates_cnt       INTEGER NOT NULL,
            created_at       TIMESTAMP NOT NULL
        "),
        Migration::add_table(20141021103504, "crates_keywords", "
            crate_id         INTEGER NOT NULL,
            keyword_id       INTEGER NOT NULL
        "),
        foreign_key(20141021103505, "crates_keywords", "crate_id", "crates (id)"),
        foreign_key(20141021103506, "crates_keywords", "keyword_id",
                    "keywords (id)"),
        index(20141021103507, "crates_keywords", "crate_id"),
        index(20141021103508, "crates_keywords", "keyword_id"),
        index(20141021103509, "keywords", "keyword"),
        index(20141021103510, "keywords", "crates_cnt"),
        Migration::add_column(20141022110441, "dependencies", "target", "varchar"),
        Migration::add_column(20141023180230, "crates", "license", "varchar"),
        Migration::add_column(20141023180231, "crates", "repository", "varchar"),

        Migration::new(20141112082527, |tx| {
            try!(tx.execute("ALTER TABLE users DROP CONSTRAINT IF \
                             EXISTS users_email_key", &[]));
            Ok(())

        }, |_| Ok(())),
        Migration::add_column(20141120162357, "dependencies", "kind", "INTEGER"),
        Migration::new(20141121191309, |tx| {
            try!(tx.execute("ALTER TABLE crates DROP CONSTRAINT \
                             packages_name_key", &[]));
            try!(tx.execute("CREATE UNIQUE INDEX index_crates_name \
                             ON crates (lower(name))", &[]));
            Ok(())

        }, |tx| {
            try!(tx.execute("DROP INDEX index_crates_name", &[]));
            try!(tx.execute("ALTER TABLE crates ADD CONSTRAINT packages_name_key \
                             UNIQUE (name)", &[]));
            Ok(())
        }),
        Migration::new(20150209202206, |tx| {
            try!(fix_duplicate_crate_owners(tx));
            try!(tx.execute("ALTER TABLE crate_owners ADD CONSTRAINT \
                             crate_owners_unique_user_per_crate \
                             UNIQUE (user_id, crate_id)", &[]));
            Ok(())
        }, |tx| {
            try!(tx.execute("ALTER TABLE crate_owners DROP CONSTRAINT \
                             crate_owners_unique_user_per_crate", &[]));
            Ok(())
        }),
        Migration::new(20150319224700, |tx| {
            try!(tx.execute("
                CREATE FUNCTION canon_crate_name(text) RETURNS text AS $$
                    SELECT replace(lower($1), '-', '_')
                $$ LANGUAGE SQL
            ", &[]));
            Ok(())
        }, |tx| {
            try!(tx.execute("DROP FUNCTION canon_crate_name(text)", &[]));
            Ok(())
        }),
        Migration::new(20150319224701, |tx| {
            try!(tx.execute("DROP INDEX index_crates_name", &[]));
            try!(tx.execute("CREATE UNIQUE INDEX index_crates_name \
                             ON crates (canon_crate_name(name))", &[]));
            Ok(())
        }, |tx| {
            try!(tx.execute("DROP INDEX index_crates_name", &[]));
            try!(tx.execute("CREATE UNIQUE INDEX index_crates_name \
                             ON crates (lower(name))", &[]));
            Ok(())
        }),
        Migration::new(20150320174400, |tx| {
            try!(tx.execute("CREATE INDEX index_keywords_lower_keyword ON keywords (lower(keyword))",
                            &[]));
            Ok(())
        }, |tx| {
            try!(tx.execute("DROP INDEX index_keywords_lower_keyword", &[]));
            Ok(())
        }),
        Migration::add_column(20150715170350, "crate_owners", "owner_kind",
                              "INTEGER NOT NULL DEFAULT 0"),
        Migration::run(20150804170127,
            "ALTER TABLE crate_owners ALTER owner_kind DROP DEFAULT",
            "ALTER TABLE crate_owners ALTER owner_kind SET DEFAULT 0",
        ),
        Migration::add_table(20150804170128, "teams", "
            id            SERIAL PRIMARY KEY,
            login         VARCHAR NOT NULL UNIQUE,
            github_id     INTEGER NOT NULL UNIQUE,
            name          VARCHAR,
            avatar        VARCHAR
        "),
        Migration::run(20150804170129,
            "ALTER TABLE crate_owners RENAME user_id TO owner_id",
            "ALTER TABLE crate_owners RENAME owner_id TO user_id",
        ),
        undo_foreign_key(20150804170130, "crate_owners", "user_id",
                         "owner_id", "users (id)"),
        Migration::new(20150818112907, |tx| {
            try!(tx.execute("ALTER TABLE crate_owners DROP CONSTRAINT \
                             crate_owners_unique_user_per_crate", &[]));
            try!(tx.execute("ALTER TABLE crate_owners ADD CONSTRAINT \
                             crate_owners_unique_owner_per_crate \
                             UNIQUE (owner_id, crate_id, owner_kind)", &[]));
            Ok(())
        }, |tx| {
            try!(tx.execute("ALTER TABLE crate_owners DROP CONSTRAINT \
                             crate_owners_unique_owner_per_crate", &[]));
            try!(tx.execute("ALTER TABLE crate_owners ADD CONSTRAINT \
                             crate_owners_unique_user_per_crate \
                             UNIQUE (owner_id, crate_id)", &[]));
            Ok(())
        }),
        Migration::run(20151120121106,
            "ALTER TABLE crates ALTER updated_at SET DEFAULT NOW()",
            "ALTER TABLE crates ALTER updated_at DROP DEFAULT",
        ),
        Migration::run(20151120121502,
            "ALTER TABLE crates ALTER created_at SET DEFAULT NOW()",
            "ALTER TABLE crates ALTER created_at DROP DEFAULT",
        ),
        Migration::new(20151120121734, |tx| {
            try!(tx.execute("ALTER TABLE crates ALTER downloads SET DEFAULT 0", &[]));
            try!(tx.execute("ALTER TABLE crates ALTER max_version SET DEFAULT '0.0.0'", &[]));
            Ok(())
        }, |tx| {
            try!(tx.execute("ALTER TABLE crates ALTER downloads DROP DEFAULT", &[]));
            try!(tx.execute("ALTER TABLE crates ALTER max_version DROP DEFAULT", &[]));
            Ok(())
        }),
        Migration::new(20151120160247, |tx| {
            try!(tx.batch_execute("
            CREATE FUNCTION set_updated_at() RETURNS trigger AS $$
            begin
                new.updated_at := NOW();
                return new;
            end
            $$ LANGUAGE plpgsql;

            CREATE TRIGGER trigger_crates_set_updated_at BEFORE UPDATE
            ON crates
            FOR EACH ROW
            WHEN (NEW.updated_at IS NOT DISTINCT FROM OLD.updated_at)
            EXECUTE PROCEDURE set_updated_at();
            "));
            Ok(())
        }, |tx| {
            try!(tx.execute("DROP TRIGGER trigger_crates_set_updated_at
                                       ON crates", &[]));
            try!(tx.execute("DROP FUNCTION set_updated_at()", &[]));
            Ok(())
        }),
        Migration::new(20151120161613, |tx| {
            try!(tx.batch_execute("
                ALTER TABLE versions ALTER downloads SET DEFAULT 0;
                ALTER TABLE versions ALTER created_at SET DEFAULT NOW();
                ALTER TABLE versions ALTER updated_at SET DEFAULT NOW();
            "));
            Ok(())
        }, |tx| {
            try!(tx.batch_execute("
                ALTER TABLE versions ALTER downloads DROP DEFAULT;
                ALTER TABLE versions ALTER created_at DROP DEFAULT;
                ALTER TABLE versions ALTER updated_at DROP DEFAULT;
            "));
            Ok(())
        }),
        Migration::new(20151120163708, |tx| {
            try!(tx.batch_execute("
                ALTER TABLE version_downloads ALTER counted SET DEFAULT 0;
                ALTER TABLE version_downloads ALTER processed SET DEFAULT 'f';
                ALTER TABLE version_downloads ALTER date SET DEFAULT DATE(NOW());
            "));
            Ok(())
        }, |tx| {
            try!(tx.batch_execute("
                ALTER TABLE version_downloads ALTER counted DROP DEFAULT;
                ALTER TABLE version_downloads ALTER processed DROP DEFAULT;
                ALTER TABLE version_downloads ALTER date DROP DEFAULT;
            "));
            Ok(())
        }),
    ];
    // NOTE: Generate a new id via `date +"%Y%m%d%H%M%S"`

    let mut seen = HashSet::new();
    for m in migrations.iter() {
        if !seen.insert(m.version()) {
            panic!("duplicate id: {}", m.version());
        }
    }
    return migrations;

    fn foreign_key(id: i64, table: &str, column: &str,
                   references: &str) -> Migration {
        let add = format!("ALTER TABLE {table} ADD CONSTRAINT fk_{table}_{col}
                                 FOREIGN KEY ({col}) REFERENCES {reference}",
                          table = table, col = column, reference = references);
        let rm = format!("ALTER TABLE {table} DROP CONSTRAINT fk_{table}_{col}",
                          table = table, col = column);
        Migration::run(id, &add, &rm)
    }

    fn undo_foreign_key(id: i64, table: &str,
                        column: &str,
                        real_column: &str,
                        references: &str) -> Migration {
        let add = format!("ALTER TABLE {table} ADD CONSTRAINT fk_{table}_{col}
                           FOREIGN KEY ({real_col}) REFERENCES {reference}",
                          table = table, col = column, reference = references,
                          real_col = real_column);
        let rm = format!("ALTER TABLE {table} DROP CONSTRAINT fk_{table}_{col}",
                         table = table, col = column);
        Migration::run(id, &rm, &add)
    }

    fn index(id: i64, table: &str, column: &str) -> Migration {
        let add = format!("CREATE INDEX index_{table}_{column}
                           ON {table} ({column})",
                          table = table, column = column);
        let rm = format!("DROP INDEX index_{table}_{column}",
                         table = table, column = column);
        Migration::run(id, &add, &rm)
    }
}

// DO NOT UPDATE OR USE FOR NEW MIGRATIONS
fn fix_duplicate_crate_owners(tx: &postgres::Transaction) -> postgres::Result<()> {
    let v: Vec<(i32, i32)> = {
        let stmt = try!(tx.prepare("SELECT user_id, crate_id
                                      FROM crate_owners
                                     GROUP BY user_id, crate_id
                                    HAVING COUNT(*) > 1"));
        try!(stmt.query(&[])).iter().map(|row| {
            (row.get("user_id"), row.get("crate_id"))
        }).collect()
    };
    for &(user_id, crate_id) in v.iter() {
        let stmt = try!(tx.prepare("SELECT id FROM crate_owners
                                    WHERE user_id = $1 AND crate_id = $2
                                    ORDER BY created_at ASC
                                    OFFSET 1"));
        for row in try!(stmt.query(&[&user_id, &crate_id])) {
            let id: i32 = row.get("id");
            try!(tx.execute("DELETE FROM crate_owners WHERE id = $1", &[&id]));
        }
    }
    Ok(())
}
