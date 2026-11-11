//! OutLayer entrypoint (wasm32-wasip1).
//!
//! * stdin  — `RunInput` JSON (public; its hash is in the TDX attestation)
//! * secret — `STRATEGY_PARAMS` (sealed `Params` JSON; OutLayer decrypts it
//!   only inside the enclave). Checked as env var first, then as a
//!   `{"input": ..., "secrets": {"STRATEGY_PARAMS": ...}}` stdin wrapper,
//!   so it works with either secret-injection convention.
//! * stdout — `RunOutput` JSON (its hash is in the TDX attestation)
//!
//! Also runs natively for local testing:
//!   echo '<RunInput json>' | STRATEGY_PARAMS='<Params json>' cargo run -p vault-strategy

use std::io::Read;
use vault_strategy::{run, Params, RunInput};

fn main() {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .expect("read stdin");
    let raw = raw.trim();

    // Secrets: env var first, then stdin wrapper.
    let (input_json, params_json): (String, String) = match std::env::var("STRATEGY_PARAMS") {
        Ok(p) => (raw.to_string(), p),
        Err(_) => {
            let v: serde_json::Value =
                serde_json::from_str(raw).expect("stdin is not valid JSON");
            let params = v
                .get("secrets")
                .and_then(|s| s.get("STRATEGY_PARAMS"))
                .map(|p| {
                    if p.is_string() {
                        p.as_str().unwrap().to_string()
                    } else {
                        p.to_string()
                    }
                })
                .expect("no STRATEGY_PARAMS secret (env or stdin.secrets)");
            let input = v.get("input").cloned().unwrap_or(v.clone());
            (input.to_string(), params)
        }
    };

    let input: RunInput = serde_json::from_str(&input_json).expect("bad RunInput");
    let params: Params = serde_json::from_str(&params_json).expect("bad Params");

    // Salt from the TEE's entropy (WASI random_get in the enclave).
    let mut salt = [0u8; 32];
    getrandom::getrandom(&mut salt).expect("entropy");

    let out = run(&params, &input, salt);
    println!("{}", serde_json::to_string(&out).expect("serialize output"));
}
