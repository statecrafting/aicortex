//! The aicortex binary (spec 010 B-3, extended by spec 015).

#![forbid(unsafe_code)]

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let env = rahi_cli::process_env();
    let mut code = rahi_cli::run_with::<aicortex::Aicortex>(&args, &env);
    if args.as_slice() == ["preflight"] {
        let embedding = aicortex::embedding_preflight(&env);
        if code == 0 {
            code = embedding;
        }
    }
    std::process::exit(code)
}
