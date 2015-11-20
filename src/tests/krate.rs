use std::collections::HashMap;
use std::error::Error;
use std::io::prelude::*;
use std::fs::{self, File};
use std::iter::repeat;

use conduit::{Handler, Request, Method};
use conduit_test::MockRequest;
use git2;
use rustc_serialize::{json, Decoder};
use semver;

use cargo_registry::dependency::EncodableDependency;
use cargo_registry::download::EncodableVersionDownload;
use cargo_registry::krate::{Crate, EncodableCrate};
use cargo_registry::upload as u;
use cargo_registry::user::EncodableUser;
use cargo_registry::version::EncodableVersion;

#[derive(RustcDecodable)]
struct CrateList { crates: Vec<EncodableCrate>, meta: CrateMeta }
#[derive(RustcDecodable)]
struct VersionsList { versions: Vec<EncodableVersion> }
#[derive(RustcDecodable)]
struct CrateMeta { total: i32 }
#[derive(RustcDecodable)]
struct GitCrate { name: String, vers: String, deps: Vec<String>, cksum: String }
#[derive(RustcDecodable)]
struct GoodCrate { krate: EncodableCrate }
#[derive(RustcDecodable)]
struct CrateResponse { krate: EncodableCrate, versions: Vec<EncodableVersion> }
#[derive(RustcDecodable)]
struct Deps { dependencies: Vec<EncodableDependency> }
#[derive(RustcDecodable)]
struct RevDeps { dependencies: Vec<EncodableDependency>, meta: CrateMeta }
#[derive(RustcDecodable)]
struct Downloads { version_downloads: Vec<EncodableVersionDownload> }

#[test]
fn index() {
    let (_b, _app, mut middle) = ::app();
    let mut req = MockRequest::new(Method::Get, "/api/v1/crates");
    let mut response = ok_resp!(middle.call(&mut req));
    let json: CrateList = ::json(&mut response);
    assert_eq!(json.crates.len(), 0);
    assert_eq!(json.meta.total, 0);

    let krate = ::krate("foo");
    middle.add(::middleware::MockUser(::user("foo")));
    middle.add(::middleware::MockCrate(krate.clone()));
    let mut response = ok_resp!(middle.call(&mut req));
    let json: CrateList = ::json(&mut response);
    assert_eq!(json.crates.len(), 1);
    assert_eq!(json.meta.total, 1);
    assert_eq!(json.crates[0].name, krate.name);
    assert_eq!(json.crates[0].id, krate.name);
}

#[test]
fn index_queries() {
    let (_b, app, middle) = ::app();

    let mut req = ::req(app, Method::Get, "/api/v1/crates");
    let u = ::mock_user(&mut req, ::user("foo"));
    let mut krate = ::krate("foo");
    krate.keywords.push("kw1".to_string());
    krate.readme = Some("readme".to_string());
    krate.description = Some("description".to_string());
    ::mock_crate(&mut req, krate);
    let mut krate2 = ::krate("BAR");
    krate2.keywords.push("KW1".to_string());
    ::mock_crate(&mut req, krate2);

    let mut response = ok_resp!(middle.call(req.with_query("q=baz")));
    assert_eq!(::json::<CrateList>(&mut response).meta.total, 0);

    // All of these fields should be indexed/searched by the queries
    let mut response = ok_resp!(middle.call(req.with_query("q=foo")));
    assert_eq!(::json::<CrateList>(&mut response).meta.total, 1);
    let mut response = ok_resp!(middle.call(req.with_query("q=kw1")));
    assert_eq!(::json::<CrateList>(&mut response).meta.total, 2);
    let mut response = ok_resp!(middle.call(req.with_query("q=readme")));
    assert_eq!(::json::<CrateList>(&mut response).meta.total, 1);
    let mut response = ok_resp!(middle.call(req.with_query("q=description")));
    assert_eq!(::json::<CrateList>(&mut response).meta.total, 1);

    let query = format!("user_id={}", u.id);
    let mut response = ok_resp!(middle.call(req.with_query(&query)));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 2);
    let mut response = ok_resp!(middle.call(req.with_query("user_id=0")));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 0);

    let mut response = ok_resp!(middle.call(req.with_query("letter=F")));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 1);
    let mut response = ok_resp!(middle.call(req.with_query("letter=B")));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 1);
    let mut response = ok_resp!(middle.call(req.with_query("letter=b")));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 1);
    let mut response = ok_resp!(middle.call(req.with_query("letter=c")));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 0);

    let mut response = ok_resp!(middle.call(req.with_query("keyword=kw1")));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 2);
    let mut response = ok_resp!(middle.call(req.with_query("keyword=KW1")));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 2);
    let mut response = ok_resp!(middle.call(req.with_query("keyword=kw2")));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 0);
}

#[test]
fn show() {
    let (_b, _app, mut middle) = ::app();
    let mut krate = ::krate("foo");
    krate.description = Some(format!("description"));
    krate.documentation = Some(format!("https://example.com"));
    krate.homepage = Some(format!("http://example.com"));
    middle.add(::middleware::MockUser(::user("foo")));
    middle.add(::middleware::MockCrate(krate.clone()));
    let mut req = MockRequest::new(Method::Get,
                                   &format!("/api/v1/crates/{}", krate.name));
    let mut response = ok_resp!(middle.call(&mut req));
    let json: CrateResponse = ::json(&mut response);
    assert_eq!(json.krate.name, krate.name);
    assert_eq!(json.krate.id, krate.name);
    assert_eq!(json.krate.description, krate.description);
    assert_eq!(json.krate.homepage, krate.homepage);
    assert_eq!(json.krate.documentation, krate.documentation);
    let versions = json.krate.versions.as_ref().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(json.versions.len(), 1);
    assert_eq!(json.versions[0].id, versions[0]);
    assert_eq!(json.versions[0].krate, json.krate.id);
    assert_eq!(json.versions[0].num, "1.0.0".to_string());
    let suffix = "/api/v1/crates/foo/1.0.0/download";
    assert!(json.versions[0].dl_path.ends_with(suffix),
            "bad suffix {}", json.versions[0].dl_path);
}

#[test]
fn versions() {
    let (_b, app, middle) = ::app();
    let mut req = ::req(app, Method::Get, "/api/v1/crates/foo/versions");
    ::mock_user(&mut req, ::user("foo"));
    ::mock_crate(&mut req, ::krate("foo"));
    let mut response = ok_resp!(middle.call(&mut req));
    let json: VersionsList = ::json(&mut response);
    assert_eq!(json.versions.len(), 1);
}

#[test]
fn new_wrong_token() {
    let (_b, app, middle) = ::app();
    let mut req = ::new_req(app.clone(), "foo", "1.0.0");
    bad_resp!(middle.call(&mut req));
    drop(req);

    let mut req = ::new_req(app.clone(), "foo", "1.0.0");
    req.header("Authorization", "bad");
    bad_resp!(middle.call(&mut req));
    drop(req);

    let mut req = ::new_req(app, "foo", "1.0.0");
    ::mock_user(&mut req, ::user("foo"));
    ::logout(&mut req);
    req.header("Authorization", "bad");
    bad_resp!(middle.call(&mut req));
}

#[test]
fn new_bad_names() {
    fn bad_name(name: &str) {
        println!("testing: `{}`", name);
        let (_b, app, middle) = ::app();
        let mut req = ::new_req(app, name, "1.0.0");
        ::mock_user(&mut req, ::user("foo"));
        ::logout(&mut req);
        let json = bad_resp!(middle.call(&mut req));
        assert!(json.errors[0].detail.contains("invalid crate name"),
                "{:?}", json.errors);
    }

    bad_name("");
    bad_name("foo bar");
}

#[test]
fn new_krate() {
    let (_b, app, middle) = ::app();
    let mut req = ::new_req(app, "foo", "1.0.0");
    let user = ::mock_user(&mut req, ::user("foo"));
    ::logout(&mut req);
    req.header("Authorization", &user.api_token);
    let mut response = ok_resp!(middle.call(&mut req));
    let json: GoodCrate = ::json(&mut response);
    assert_eq!(json.krate.name, "foo");
    assert_eq!(json.krate.max_version, "1.0.0");
}

#[test]
fn new_krate_weird_version() {
    let (_b, app, middle) = ::app();
    let mut req = ::new_req(app, "foo", "0.0.0-pre");
    let user = ::mock_user(&mut req, ::user("foo"));
    ::logout(&mut req);
    req.header("Authorization", &user.api_token);
    let mut response = ok_resp!(middle.call(&mut req));
    let json: GoodCrate = ::json(&mut response);
    assert_eq!(json.krate.name, "foo");
    assert_eq!(json.krate.max_version, "0.0.0-pre");
}

#[test]
fn new_krate_with_dependency() {
    let (_b, app, middle) = ::app();
    let dep = u::CrateDependency {
        name: u::CrateName("foo".to_string()),
        optional: false,
        default_features: true,
        features: Vec::new(),
        version_req: u::CrateVersionReq(semver::VersionReq::parse(">= 0").unwrap()),
        target: None,
        kind: None,
    };
    let mut req = ::new_req_full(app, ::krate("new"), "1.0.0", vec![dep]);
    ::mock_user(&mut req, ::user("foo"));
    ::mock_crate(&mut req, ::krate("foo"));
    let mut response = ok_resp!(middle.call(&mut req));
    ::json::<GoodCrate>(&mut response);
}

#[test]
fn new_krate_twice() {
    let (_b, app, middle) = ::app();
    let mut krate = ::krate("foo");
    krate.description = Some("description".to_string());
    let mut req = ::new_req_full(app, krate.clone(), "2.0.0", Vec::new());
    ::mock_user(&mut req, ::user("foo"));
    ::mock_crate(&mut req, ::krate("foo"));
    let mut response = ok_resp!(middle.call(&mut req));
    let json: GoodCrate = ::json(&mut response);
    assert_eq!(json.krate.name, krate.name);
    assert_eq!(json.krate.description, krate.description);
}

#[test]
fn new_krate_wrong_user() {
    let (_b, app, middle) = ::app();

    let mut req = ::new_req(app, "foo", "2.0.0");

    // Create the 'foo' crate with one user
    ::mock_user(&mut req, ::user("foo"));
    ::mock_crate(&mut req, ::krate("foo"));

    // But log in another
    ::mock_user(&mut req, ::user("bar"));

    let json = bad_resp!(middle.call(&mut req));
    assert!(json.errors[0].detail.contains("another user"),
            "{:?}", json.errors);
}

#[test]
fn new_krate_bad_name() {
    let (_b, app, middle) = ::app();

    {
        let mut req = ::new_req(app.clone(), "snow☃", "2.0.0");
        ::mock_user(&mut req, ::user("foo"));
        let json = bad_resp!(middle.call(&mut req));
        assert!(json.errors[0].detail.contains("invalid crate name"),
                "{:?}", json.errors);
    }
    {
        let mut req = ::new_req(app, "áccênts", "2.0.0");
        ::mock_user(&mut req, ::user("foo"));
        let json = bad_resp!(middle.call(&mut req));
        assert!(json.errors[0].detail.contains("invalid crate name"),
                "{:?}", json.errors);
    }
}

#[test]
fn new_crate_owner() {
    #[derive(RustcDecodable)] struct O { ok: bool }

    let (_b, app, middle) = ::app();

    // Create a crate under one user
    let mut req = ::new_req(app.clone(), "foo", "1.0.0");
    let u2 = ::mock_user(&mut req, ::user("bar"));
    ::mock_user(&mut req, ::user("foo"));
    let mut response = ok_resp!(middle.call(&mut req));
    ::json::<GoodCrate>(&mut response);

    // Flag the second user as an owner
    let body = r#"{"users":["bar"]}"#;
    let mut response = ok_resp!(middle.call(req.with_path("/api/v1/crates/foo/owners")
                                               .with_method(Method::Put)
                                               .with_body(body.as_bytes())));
    assert!(::json::<O>(&mut response).ok);
    bad_resp!(middle.call(req.with_path("/api/v1/crates/foo/owners")
                             .with_method(Method::Put)
                             .with_body(body.as_bytes())));

    // Make sure this shows up as one of their crates.
    let query = format!("user_id={}", u2.id);
    let mut response = ok_resp!(middle.call(req.with_path("/api/v1/crates")
                                               .with_method(Method::Get)
                                               .with_query(&query)));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 1);

    // And upload a new crate as the first user
    let body = ::new_req_body(::krate("foo"), "2.0.0", Vec::new());
    req.mut_extensions().insert(u2);
    let mut response = ok_resp!(middle.call(req.with_path("/api/v1/crates/new")
                                               .with_method(Method::Put)
                                               .with_body(&body)));
    ::json::<GoodCrate>(&mut response);
}

#[test]
fn valid_feature_names() {
    assert!(Crate::valid_feature_name("foo"));
    assert!(!Crate::valid_feature_name(""));
    assert!(!Crate::valid_feature_name("/"));
    assert!(!Crate::valid_feature_name("%/%"));
    assert!(Crate::valid_feature_name("a/a"));
}

#[test]
fn new_krate_too_big() {
    let (_b, app, middle) = ::app();
    let mut req = ::new_req(app, "foo", "1.0.0");
    ::mock_user(&mut req, ::user("foo"));
    req.with_body(repeat("a").take(1000 * 1000).collect::<String>().as_bytes());
    bad_resp!(middle.call(&mut req));
}

#[test]
fn new_krate_duplicate_version() {
    let (_b, app, middle) = ::app();
    let mut req = ::new_req(app, "foo", "1.0.0");
    ::mock_user(&mut req, ::user("foo"));
    ::mock_crate(&mut req, ::krate("foo"));
    let json = bad_resp!(middle.call(&mut req));
    assert!(json.errors[0].detail.contains("already uploaded"),
            "{:?}", json.errors);
}

#[test]
fn new_crate_similar_name() {
    let (_b, app, middle) = ::app();
    let mut req = ::new_req(app, "foo", "1.1.0");
    ::mock_user(&mut req, ::user("foo"));
    ::mock_crate(&mut req, ::krate("Foo"));
    let json = bad_resp!(middle.call(&mut req));
    assert!(json.errors[0].detail.contains("previously named"),
            "{:?}", json.errors);
}

#[test]
fn new_crate_similar_name_hyphen() {
    {
        let (_b, app, middle) = ::app();
        let mut req = ::new_req(app, "foo-bar", "1.1.0");
        ::mock_user(&mut req, ::user("foo"));
        ::mock_crate(&mut req, ::krate("foo_bar"));
        let json = bad_resp!(middle.call(&mut req));
        assert!(json.errors[0].detail.contains("previously named"),
                "{:?}", json.errors);
    }
    {
        let (_b, app, middle) = ::app();
        let mut req = ::new_req(app, "foo_bar", "1.1.0");
        ::mock_user(&mut req, ::user("foo"));
        ::mock_crate(&mut req, ::krate("foo-bar"));
        let json = bad_resp!(middle.call(&mut req));
        assert!(json.errors[0].detail.contains("previously named"),
                "{:?}", json.errors);
    }
}

#[test]
fn new_krate_git_upload() {
    let (_b, app, middle) = ::app();
    let mut req = ::new_req(app, "foo", "1.0.0");
    ::mock_user(&mut req, ::user("foo"));
    let mut response = ok_resp!(middle.call(&mut req));
    ::json::<GoodCrate>(&mut response);

    let path = ::git::checkout().join("3/f/foo");
    assert!(path.exists());
    let mut contents = String::new();
    File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
    let p: GitCrate = json::decode(&contents).unwrap();
    assert_eq!(p.name, "foo");
    assert_eq!(p.vers, "1.0.0");
    assert!(p.deps.is_empty());
    assert_eq!(p.cksum,
               "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
}

#[test]
fn new_krate_git_upload_appends() {
    let (_b, app, middle) = ::app();
    let path = ::git::checkout().join("3/f/foo");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    File::create(&path).unwrap().write_all(
        br#"{"name":"FOO","vers":"0.0.1","deps":[],"cksum":"3j3"}
"#).unwrap();

    let mut req = ::new_req(app, "FOO", "1.0.0");
    ::mock_user(&mut req, ::user("foo"));
    let mut response = ok_resp!(middle.call(&mut req));
    ::json::<GoodCrate>(&mut response);

    let mut contents = String::new();
    File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
    let mut lines = contents.lines();
    let p1: GitCrate = json::decode(lines.next().unwrap().trim()).unwrap();
    let p2: GitCrate = json::decode(lines.next().unwrap().trim()).unwrap();
    assert!(lines.next().is_none());
    assert_eq!(p1.name, "FOO");
    assert_eq!(p1.vers, "0.0.1");
    assert!(p1.deps.is_empty());
    assert_eq!(p2.name, "FOO");
    assert_eq!(p2.vers, "1.0.0");
    assert!(p2.deps.is_empty());
}

#[test]
fn new_krate_git_upload_with_conflicts() {
    let (_b, app, middle) = ::app();

    {
        let repo = git2::Repository::open(&::git::bare()).unwrap();
        let target = repo.head().unwrap().target().unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.find_commit(target).unwrap();
        let tree = repo.find_tree(parent.tree_id()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "empty commit", &tree,
                    &[&parent]).unwrap();
    }

    let mut req = ::new_req(app, "foo", "1.0.0");
    ::mock_user(&mut req, ::user("foo"));
    let mut response = ok_resp!(middle.call(&mut req));
    ::json::<GoodCrate>(&mut response);
}

#[test]
fn new_krate_dependency_missing() {
    let (_b, app, middle) = ::app();
    let dep = u::CrateDependency {
        optional: false,
        default_features: true,
        name: u::CrateName("bar".to_string()),
        features: Vec::new(),
        version_req: u::CrateVersionReq(semver::VersionReq::parse(">= 0.0.0").unwrap()),
        target: None,
        kind: None,
    };
    let mut req = ::new_req_full(app, ::krate("foo"), "1.0.0", vec![dep]);
    ::mock_user(&mut req, ::user("foo"));
    let mut response = ok_resp!(middle.call(&mut req));
    let json = ::json::<::Bad>(&mut response);
    assert!(json.errors[0].detail
                .contains("no known crate named `bar`"));
}

#[test]
fn summary_doesnt_die() {
    let (_b, _app, middle) = ::app();
    let mut req = MockRequest::new(Method::Get, "/summary");
    ok_resp!(middle.call(&mut req));
}

#[test]
fn download() {
    let (_b, app, middle) = ::app();
    let mut req = ::req(app, Method::Get, "/api/v1/crates/foo/1.0.0/download");
    ::new_mock_user(&mut req, ::user("foo"));
    ::new_mock_crate(&mut req, ::krate("foo"));
    let mut resp = t_resp!(middle.call(&mut req));
    let mut s = String::new();
    resp.body.read_to_string(&mut s).unwrap();
    println!("{}", s);
    assert_eq!(resp.status.0, 302);

    req.with_path("/api/v1/crates/foo/1.0.0/downloads");
    let mut resp = ok_resp!(middle.call(&mut req));
    let downloads = ::json::<Downloads>(&mut resp);
    assert_eq!(downloads.version_downloads.len(), 1);

    req.with_path("/api/v1/crates/FOO/1.0.0/download");
    let resp = t_resp!(middle.call(&mut req));
    assert_eq!(resp.status.0, 302);

    req.with_path("/api/v1/crates/FOO/1.0.0/downloads");
    let mut resp = ok_resp!(middle.call(&mut req));
    let downloads = ::json::<Downloads>(&mut resp);
    assert_eq!(downloads.version_downloads.len(), 1);
}

#[test]
fn download_bad() {
    let (_b, _app, mut middle) = ::app();
    let user = ::user("foo");
    let krate = ::krate("foo");
    middle.add(::middleware::MockUser(user.clone()));
    middle.add(::middleware::MockCrate(krate.clone()));
    let rel = format!("/api/v1/crates/{}/0.1.0/download", krate.name);
    let mut req = MockRequest::new(Method::Get, &rel);
    let mut response = ok_resp!(middle.call(&mut req));
    ::json::<::Bad>(&mut response);
}

#[test]
fn dependencies() {
    let (_b, app, middle) = ::app();

    let mut req = ::req(app, Method::Get, "/api/v1/crates/foo/1.0.0/dependencies");
    ::mock_user(&mut req, ::user("foo"));
    let (_, v) = ::mock_crate(&mut req, ::krate("foo"));
    let (c, _) = ::mock_crate(&mut req, ::krate("bar"));
    ::mock_dep(&mut req, &v, &c, None);

    let mut response = ok_resp!(middle.call(&mut req));
    let deps = ::json::<Deps>(&mut response);
    assert_eq!(deps.dependencies[0].crate_id, "bar");

    req.with_path("/api/v1/crates/foo/1.0.2/dependencies");
    let mut response = ok_resp!(middle.call(&mut req));
    ::json::<::Bad>(&mut response);
}

#[test]
fn following() {
    #[derive(RustcDecodable)] struct F { following: bool }
    #[derive(RustcDecodable)] struct O { ok: bool }

    let (_b, app, middle) = ::app();
    let mut req = ::req(app, Method::Get, "/api/v1/crates/foo/following");
    ::mock_user(&mut req, ::user("foo"));
    ::mock_crate(&mut req, ::krate("foo"));

    let mut response = ok_resp!(middle.call(&mut req));
    assert!(!::json::<F>(&mut response).following);

    req.with_path("/api/v1/crates/foo/follow")
       .with_method(Method::Put);
    let mut response = ok_resp!(middle.call(&mut req));
    assert!(::json::<O>(&mut response).ok);
    let mut response = ok_resp!(middle.call(&mut req));
    assert!(::json::<O>(&mut response).ok);

    req.with_path("/api/v1/crates/foo/following")
       .with_method(Method::Get);
    let mut response = ok_resp!(middle.call(&mut req));
    assert!(::json::<F>(&mut response).following);

    req.with_path("/api/v1/crates")
       .with_query("following=1");
    let mut response = ok_resp!(middle.call(&mut req));
    let l = ::json::<CrateList>(&mut response);
    assert_eq!(l.crates.len(), 1);

    req.with_path("/api/v1/crates/foo/follow")
       .with_method(Method::Delete);
    let mut response = ok_resp!(middle.call(&mut req));
    assert!(::json::<O>(&mut response).ok);
    let mut response = ok_resp!(middle.call(&mut req));
    assert!(::json::<O>(&mut response).ok);

    req.with_path("/api/v1/crates/foo/following")
       .with_method(Method::Get);
    let mut response = ok_resp!(middle.call(&mut req));
    assert!(!::json::<F>(&mut response).following);

    req.with_path("/api/v1/crates")
       .with_query("following=1")
       .with_method(Method::Get);
    let mut response = ok_resp!(middle.call(&mut req));
    assert_eq!(::json::<CrateList>(&mut response).crates.len(), 0);
}

#[test]
fn owners() {
    #[derive(RustcDecodable)] struct R { users: Vec<EncodableUser> }
    #[derive(RustcDecodable)] struct O { ok: bool }

    let (_b, app, middle) = ::app();
    let mut req = ::req(app, Method::Get, "/api/v1/crates/foo/owners");
    let other = ::user("foobar");
    ::mock_user(&mut req, other);
    ::mock_user(&mut req, ::user("foo"));
    ::mock_crate(&mut req, ::krate("foo"));

    let mut response = ok_resp!(middle.call(&mut req));
    let r: R = ::json(&mut response);
    assert_eq!(r.users.len(), 1);

    let mut response = ok_resp!(middle.call(req.with_method(Method::Get)));
    let r: R = ::json(&mut response);
    assert_eq!(r.users.len(), 1);

    let body = r#"{"users":["foobar"]}"#;
    let mut response = ok_resp!(middle.call(req.with_method(Method::Put)
                                               .with_body(body.as_bytes())));
    assert!(::json::<O>(&mut response).ok);

    let mut response = ok_resp!(middle.call(req.with_method(Method::Get)));
    let r: R = ::json(&mut response);
    assert_eq!(r.users.len(), 2);

    let body = r#"{"users":["foobar"]}"#;
    let mut response = ok_resp!(middle.call(req.with_method(Method::Delete)
                                               .with_body(body.as_bytes())));
    assert!(::json::<O>(&mut response).ok);

    let mut response = ok_resp!(middle.call(req.with_method(Method::Get)));
    let r: R = ::json(&mut response);
    assert_eq!(r.users.len(), 1);

    let body = r#"{"users":["foo"]}"#;
    let mut response = ok_resp!(middle.call(req.with_method(Method::Delete)
                                               .with_body(body.as_bytes())));
    ::json::<::Bad>(&mut response);

    let body = r#"{"users":["foobar"]}"#;
    let mut response = ok_resp!(middle.call(req.with_method(Method::Put)
                                               .with_body(body.as_bytes())));
    assert!(::json::<O>(&mut response).ok);
}

#[test]
fn yank() {
    #[derive(RustcDecodable)] struct O { ok: bool }
    #[derive(RustcDecodable)] struct V { version: EncodableVersion }
    let (_b, app, middle) = ::app();
    let path = ::git::checkout().join("3/f/foo");

    // Upload a new crate, putting it in the git index
    let mut req = ::new_req(app, "foo", "1.0.0");
    ::mock_user(&mut req, ::user("foo"));
    let mut response = ok_resp!(middle.call(&mut req));
    ::json::<GoodCrate>(&mut response);
    let mut contents = String::new();
    File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
    assert!(contents.contains("\"yanked\":false"));

    // make sure it's not yanked
    let mut r = ok_resp!(middle.call(req.with_method(Method::Get)
                                        .with_path("/api/v1/crates/foo/1.0.0")));
    assert!(!::json::<V>(&mut r).version.yanked);

    // yank it
    let mut r = ok_resp!(middle.call(req.with_method(Method::Delete)
                                        .with_path("/api/v1/crates/foo/1.0.0/yank")));
    assert!(::json::<O>(&mut r).ok);
    let mut contents = String::new();
    File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
    assert!(contents.contains("\"yanked\":true"));
    let mut r = ok_resp!(middle.call(req.with_method(Method::Get)
                                        .with_path("/api/v1/crates/foo/1.0.0")));
    assert!(::json::<V>(&mut r).version.yanked);

    // un-yank it
    let mut r = ok_resp!(middle.call(req.with_method(Method::Put)
                                        .with_path("/api/v1/crates/foo/1.0.0/unyank")));
    assert!(::json::<O>(&mut r).ok);
    let mut contents = String::new();
    File::open(&path).unwrap().read_to_string(&mut contents).unwrap();
    assert!(contents.contains("\"yanked\":false"));
    let mut r = ok_resp!(middle.call(req.with_method(Method::Get)
                                        .with_path("/api/v1/crates/foo/1.0.0")));
    assert!(!::json::<V>(&mut r).version.yanked);
}

#[test]
fn yank_not_owner() {
    let (_b, app, middle) = ::app();
    let mut req = ::req(app, Method::Delete, "/api/v1/crates/foo/1.0.0/yank");
    ::mock_user(&mut req, ::user("foo"));
    ::mock_crate(&mut req, ::krate("foo"));
    ::mock_user(&mut req, ::user("bar"));
    let mut response = ok_resp!(middle.call(&mut req));
    ::json::<::Bad>(&mut response);
}

#[test]
fn bad_keywords() {
    let (_b, app, middle) = ::app();
    {
        let mut krate = ::krate("foo");
        krate.keywords.push("super-long-keyword-name-oh-no".to_string());
        let mut req = ::new_req_full(app.clone(), krate, "1.0.0", Vec::new());
        ::mock_user(&mut req, ::user("foo"));
        let mut response = ok_resp!(middle.call(&mut req));
        ::json::<::Bad>(&mut response);
    }
    {
        let mut krate = ::krate("foo");
        krate.keywords.push("?@?%".to_string());
        let mut req = ::new_req_full(app.clone(), krate, "1.0.0", Vec::new());
        ::mock_user(&mut req, ::user("foo"));
        let mut response = ok_resp!(middle.call(&mut req));
        ::json::<::Bad>(&mut response);
    }
    {
        let mut krate = ::krate("foo");
        krate.keywords.push("?@?%".to_string());
        let mut req = ::new_req_full(app.clone(), krate, "1.0.0", Vec::new());
        ::mock_user(&mut req, ::user("foo"));
        let mut response = ok_resp!(middle.call(&mut req));
        ::json::<::Bad>(&mut response);
    }
    {
        let mut krate = ::krate("foo");
        krate.keywords.push("áccênts".to_string());
        let mut req = ::new_req_full(app.clone(), krate, "1.0.0", Vec::new());
        ::mock_user(&mut req, ::user("foo"));
        let mut response = ok_resp!(middle.call(&mut req));
        ::json::<::Bad>(&mut response);
    }
}

#[test]
fn reverse_dependencies() {
    let (_b, app, middle) = ::app();

    let v100 = semver::Version::parse("1.0.0").unwrap();
    let v110 = semver::Version::parse("1.1.0").unwrap();
    let mut req = ::req(app, Method::Get,
                        "/api/v1/crates/c1/reverse_dependencies");
    ::mock_user(&mut req, ::user("foo"));
    let (c1, _) = ::mock_crate_vers(&mut req, ::krate("c1"), &v100);
    let (_, c2v1) = ::mock_crate_vers(&mut req, ::krate("c2"), &v100);
    let (_, c2v2) = ::mock_crate_vers(&mut req, ::krate("c2"), &v110);

    ::mock_dep(&mut req, &c2v1, &c1, None);
    ::mock_dep(&mut req, &c2v2, &c1, None);
    ::mock_dep(&mut req, &c2v2, &c1, Some("foo"));

    let mut response = ok_resp!(middle.call(&mut req));
    let deps = ::json::<RevDeps>(&mut response);
    assert_eq!(deps.dependencies.len(), 1);
    assert_eq!(deps.meta.total, 1);
    assert_eq!(deps.dependencies[0].crate_id, "c2");

    // c1 has no dependent crates.
    req.with_path("/api/v1/crates/c2/reverse_dependencies");
    let mut response = ok_resp!(middle.call(&mut req));
    let deps = ::json::<RevDeps>(&mut response);
    assert_eq!(deps.dependencies.len(), 0);
    assert_eq!(deps.meta.total, 0);
}

#[test]
fn author_license_and_description_required() {
    let (_b, app, middle) = ::app();
    ::user("foo");

    let mut req = ::req(app, Method::Put, "/api/v1/crates/new");
    let mut new_crate = u::NewCrate {
        name: u::CrateName("foo".to_string()),
        vers: u::CrateVersion(semver::Version::parse("1.0.0").unwrap()),
        features: HashMap::new(),
        deps: Vec::new(),
        authors: Vec::new(),
        description: None,
        homepage: None,
        documentation: None,
        readme: None,
        keywords: None,
        license: None,
        license_file: None,
        repository: None,
    };
    req.with_body(&::new_crate_to_body(&new_crate));
    let json = bad_resp!(middle.call(&mut req));
    assert!(json.errors[0].detail.contains("author") &&
            json.errors[0].detail.contains("description") &&
            json.errors[0].detail.contains("license"),
            "{:?}", json.errors);

    new_crate.license = Some("MIT".to_string());
    new_crate.authors.push("".to_string());
    req.with_body(&::new_crate_to_body(&new_crate));
    let json = bad_resp!(middle.call(&mut req));
    assert!(json.errors[0].detail.contains("author") &&
            json.errors[0].detail.contains("description") &&
            !json.errors[0].detail.contains("license"),
            "{:?}", json.errors);

    new_crate.license = None;
    new_crate.license_file = Some("foo".to_string());
    new_crate.authors.push("foo".to_string());
    req.with_body(&::new_crate_to_body(&new_crate));
    let json = bad_resp!(middle.call(&mut req));
    assert!(!json.errors[0].detail.contains("author") &&
            json.errors[0].detail.contains("description") &&
            !json.errors[0].detail.contains("license"),
            "{:?}", json.errors);
}
