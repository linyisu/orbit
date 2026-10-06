#[path = "../../build-support/server.rs"]
mod portable;

fn main() {
    portable::bundle();
}
