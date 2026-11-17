//! The local demo app (login + dashboard) as a standalone server, for trying Klyro by hand,
//! agent checks and the benchmark: `cargo run --example demo_app`.

#[path = "../../../tests/support/demo_app.rs"]
mod demo_app;

fn main() {
    println!("{}/login  (log in with demo / secret; Ctrl+C to stop)", demo_app::demo_app());
    loop {
        std::thread::park();
    }
}
