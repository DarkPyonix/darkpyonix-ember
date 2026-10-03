//! `ember-a2a`: the agent-side A2A tool (SPEC FR-T2). See `ember_server::a2a::client`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(ember_server::a2a::client::run(&args));
}
