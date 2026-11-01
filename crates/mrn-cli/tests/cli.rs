//! Runs the `mrn` binary offline in an isolated XDG environment.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn mrn(home: &Path) -> Command {
    let mut c = Command::cargo_bin("mrn").unwrap();
    c.env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("MRN_CONFIG", home.join("config/mrn/config.toml"))
        .env_remove("TMDB_API_KEY")
        .env_remove("COMICVINE_API_KEY")
        .current_dir(home);
    c
}

#[test]
fn rename_history_undo() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path();
    fs::create_dir_all(home.join("in")).unwrap();
    fs::write(home.join("in/The.Expanse.S02E05.1080p.mkv"), b"x").unwrap();
    fs::write(home.join("in/One Piece v01.cbz"), b"x").unwrap();

    // Without a TTY, applying needs --yes.
    mrn(home)
        .args(["rename", "in", "-o", "lib", "--offline"])
        .assert()
        .failure()
        .stderr(contains("--yes"));

    mrn(home)
        .args(["rename", "in", "-o", "lib", "--offline", "-n"])
        .assert()
        .success()
        .stdout(contains("The Expanse/Season 02/The Expanse - S02E05.mkv"))
        .stdout(contains("Dry run"));
    assert!(home.join("in/The.Expanse.S02E05.1080p.mkv").exists());

    mrn(home)
        .args(["rename", "in", "-o", "lib", "--offline", "-y"])
        .assert()
        .success()
        .stdout(contains("Moved 2 file(s)"));
    assert!(home.join("lib/The Expanse/Season 02/The Expanse - S02E05.mkv").exists());
    assert!(home.join("lib/One Piece/One Piece Vol.01.cbz").exists());

    mrn(home)
        .arg("history")
        .assert()
        .success()
        .stdout(contains("mrn rename in -o lib --offline -y"));

    mrn(home).args(["undo", "-y"]).assert().success().stdout(contains("undone"));
    assert!(home.join("in/The.Expanse.S02E05.1080p.mkv").exists());
    assert!(!home.join("lib").exists());

    mrn(home).args(["undo", "-y"]).assert().success().stdout(contains("Nothing to undo"));
}

#[test]
fn missing_tmdb_key_is_explained() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("Dune.2021.mkv"), b"x").unwrap();
    mrn(d.path())
        .args(["rename", "Dune.2021.mkv", "-n"])
        .assert()
        .failure()
        .stdout(contains("TMDB API key"));
}

#[test]
fn config_init_and_show() {
    let d = tempfile::tempdir().unwrap();
    mrn(d.path()).args(["config", "init"]).assert().success();
    mrn(d.path()).args(["config", "init"]).assert().failure().stderr(contains("--force"));
    mrn(d.path())
        .args(["config", "show"])
        .env("TMDB_API_KEY", "secret-key")
        .assert()
        .success()
        .stdout(contains("preset = \"jellyfin\""))
        .stdout(contains("********"))
        .stdout(predicates::str::contains("secret-key").not());
}

#[test]
fn info_offline() {
    let d = tempfile::tempdir().unwrap();
    let f = "[SubsPlease] Sousou no Frieren - 12 (1080p) [ABCD1234].mkv";
    fs::write(d.path().join(f), b"x").unwrap();
    mrn(d.path())
        .args(["info", f, "--offline"])
        .assert()
        .success()
        .stdout(contains("anime (detected)"))
        .stdout(contains("Sousou no Frieren - 12.mkv"));
}

#[test]
fn bad_template_is_reported() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("Dune.2021.mkv"), b"x").unwrap();
    mrn(d.path())
        .args(["rename", "Dune.2021.mkv", "--offline", "-n", "-f", "{n} {nope}"])
        .assert()
        .failure()
        .stderr(contains("unknown template field {nope}"));
}

#[test]
fn completions_and_man_page() {
    let d = tempfile::tempdir().unwrap();
    mrn(d.path())
        .args(["completions", "fish"])
        .assert()
        .success()
        .stdout(contains("complete -c mrn").and(contains("rename")));
    mrn(d.path())
        .arg("man")
        .assert()
        .success()
        .stdout(contains(".TH mrn"));
}
