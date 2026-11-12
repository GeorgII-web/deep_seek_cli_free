//! Proof of work: the site's own wasm module is run in an embedded interpreter.

use super::client::Challenge;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::json;
use wasmi::{Engine, Instance, Linker, Module, Store};

type Ctx = Store<()>;

/// The interpreter recurses deeply; the default 2 MB of a worker thread overflow.
const STACK_BYTES: usize = 64 * 1024 * 1024;

/// Finds the answer of `challenge` with the module `wasm` (blocking, CPU bound).
/// Runs on its own thread with a large stack.
pub fn solve(wasm: &[u8], challenge: &Challenge) -> Result<u64> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("pow".into())
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || run(wasm, challenge))
            .context("start the proof-of-work thread")?
            .join()
            .map_err(|_| anyhow!("proof-of-work thread panicked"))?
    })
}

fn run(wasm: &[u8], challenge: &Challenge) -> Result<u64> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm).map_err(|e| anyhow!("proof-of-work module: {e}"))?;
    let mut store: Ctx = Store::new(&engine, ());
    let instance = Linker::<()>::new(&engine)
        .instantiate_and_start(&mut store, &module)
        .map_err(|e| anyhow!("proof-of-work module: {e}"))?;

    let prefix = format!("{}_{}_", challenge.salt, challenge.expire_at);
    let (challenge_ptr, challenge_len) =
        put(&instance, &mut store, challenge.challenge.as_bytes())?;
    let (prefix_ptr, prefix_len) = put(&instance, &mut store, prefix.as_bytes())?;

    let stack = instance
        .get_typed_func::<i32, i32>(&store, "__wbindgen_add_to_stack_pointer")
        .map_err(|e| anyhow!("proof-of-work module: {e}"))?;
    let out = stack.call(&mut store, -16).map_err(|e| anyhow!("{e}"))?;
    instance
        .get_typed_func::<(i32, i32, i32, i32, i32, f64), ()>(&store, "wasm_solve")
        .map_err(|e| anyhow!("proof-of-work module: wasm_solve: {e}"))?
        .call(
            &mut store,
            (
                out,
                challenge_ptr,
                challenge_len,
                prefix_ptr,
                prefix_len,
                challenge.difficulty as f64,
            ),
        )
        .map_err(|e| anyhow!("proof-of-work run: {e}"))?;

    let mut result = [0u8; 16];
    instance
        .get_memory(&store, "memory")
        .context("proof-of-work module: no memory")?
        .read(&store, out as u32 as usize, &mut result)
        .map_err(|e| anyhow!("{e}"))?;
    let _ = stack.call(&mut store, 16);

    let found = i32::from_le_bytes(result[0..4].try_into().unwrap());
    let answer = f64::from_le_bytes(result[8..16].try_into().unwrap());
    if found == 0 || !answer.is_finite() || answer < 0.0 {
        bail!("proof of work: no answer found");
    }
    Ok(answer as u64)
}

/// Copies `bytes` into memory allocated by the module; returns pointer and length.
fn put(instance: &Instance, store: &mut Ctx, bytes: &[u8]) -> Result<(i32, i32)> {
    let len = bytes.len() as i32;
    let ptr = instance
        .get_typed_func::<(i32, i32), i32>(&*store, "__wbindgen_export_0")
        .map_err(|e| anyhow!("proof-of-work module: allocator: {e}"))?
        .call(&mut *store, (len, 1))
        .map_err(|e| anyhow!("{e}"))?;
    instance
        .get_memory(&*store, "memory")
        .context("proof-of-work module: no memory")?
        .write(&mut *store, ptr as u32 as usize, bytes)
        .map_err(|e| anyhow!("{e}"))?;
    Ok((ptr, len))
}

/// Value of the `X-DS-PoW-Response` header for the call to `target_path`.
pub fn header(challenge: &Challenge, answer: u64, target_path: &str) -> String {
    let body = json!({
        "algorithm": challenge.algorithm,
        "challenge": challenge.challenge,
        "salt": challenge.salt,
        "answer": answer,
        "signature": challenge.signature,
        "target_path": target_path,
    });
    base64::engine::general_purpose::STANDARD.encode(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deepseek::client::COMPLETION_PATH;

    fn challenge() -> Challenge {
        Challenge {
            algorithm: "DeepSeekHashV1".into(),
            challenge: "0".repeat(64),
            salt: "abc".into(),
            difficulty: 144_000,
            expire_at: 1_700_000_000_000,
            signature: "sig".into(),
        }
    }

    /// Needs the site's module: `DSC_TEST_WASM=<file> cargo test -- --ignored`.
    /// A made-up challenge has no answer; the point is that the module loads, runs
    /// the whole search without a trap and reports "not found".
    #[test]
    #[ignore]
    fn site_module_runs_a_full_search() {
        let wasm = std::fs::read(std::env::var("DSC_TEST_WASM").unwrap()).unwrap();
        let started = std::time::Instant::now();
        let err = solve(&wasm, &challenge()).unwrap_err().to_string();
        eprintln!("full search: {} ms", started.elapsed().as_millis());
        assert_eq!(err, "proof of work: no answer found");
    }

    #[test]
    fn header_is_base64_json_with_the_answer() {
        let text = base64::engine::general_purpose::STANDARD
            .decode(header(&challenge(), 42, COMPLETION_PATH))
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(value["answer"], 42);
        assert_eq!(value["target_path"], COMPLETION_PATH);
        assert_eq!(value["salt"], "abc");
    }
}
