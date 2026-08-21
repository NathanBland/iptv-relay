// Force recompilation when migration files change.
// sqlx::migrate! embeds migration files at compile time but does not
// register them as cargo build dependencies, so cargo may skip
// recompiling this crate when only migration files change.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
