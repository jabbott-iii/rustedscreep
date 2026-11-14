//! Prints the MMO loader configuration as JSON; `npm run build` injects it into the bundle.
fn main() {
    println!("{}", rustedscreep_core::config::LOADER_MMO.to_json());
}
