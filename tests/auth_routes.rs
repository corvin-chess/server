//! CV-133: auth route and migration shape asserts (always-on, no Postgres).

use std::path::PathBuf;

#[test]
fn auth_migration_sql_files_are_present_and_paired() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let up = root.join("20261124120000_passkeys_email_otps.up.sql");
    let down = root.join("20261124120000_passkeys_email_otps.down.sql");
    assert!(up.is_file(), "missing {}", up.display());
    assert!(down.is_file(), "missing {}", down.display());

    let up_sql = std::fs::read_to_string(&up).expect("read up.sql");
    let down_sql = std::fs::read_to_string(&down).expect("read down.sql");

    let up_l = up_sql.to_lowercase();
    let down_l = down_sql.to_lowercase();
    assert!(
        up_l.contains("create table") && up_l.contains("passkeys") && up_l.contains("email_otps"),
        "up migration must create passkeys and email_otps"
    );
    assert!(
        down_l.contains("drop table")
            && down_l.contains("passkeys")
            && down_l.contains("email_otps"),
        "down migration must drop passkeys and email_otps"
    );
    assert!(
        !up_l.contains("threshold")
            && !up_l.contains("fair_play")
            && !up_l.contains("password_hash"),
        "migration must not embed fair-play secrets or password hashes"
    );
}

#[test]
fn auth_source_modules_exist() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    for rel in [
        "auth/mod.rs",
        "auth/email_otp.rs",
        "auth/passkey.rs",
        "auth/session.rs",
        "auth/csrf.rs",
        "auth/mailer.rs",
        "auth/rate_limit.rs",
        "safety/mod.rs",
        "account/mod.rs",
        "account/messaging.rs",
    ] {
        let path = src.join(rel);
        assert!(path.is_file(), "missing {}", path.display());
    }
}
