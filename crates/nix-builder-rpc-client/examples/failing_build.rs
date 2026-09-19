//! Submit ONE derivation whose builder exits non-zero, through this crate,
//! and report what the daemon said. Exists to answer whether the worker
//! segfault recorded in local/audits/2026-09-07-daemonstalled-is-a-worker-
//! crash-loop.md needs this client, a CA output, or neither. Every arm is
//! chosen by argv so one binary serves all of them:
//!
//!   failing_build <ca|ia> <bash-store-path> [<builder-path>]
//!
//! `ca` mints a floating content-addressed output the way a task derivation
//! does; `ia` mints an input-addressed one the way `derivation {}` does.
//! A third argument replaces the builder with that path VERBATIM and drops
//! bash from the inputs, which is how a task derivation names its builder
//! under the stable-builder knob: `/nn-task/bin/nix-ninja-task`, mounted by
//! the daemon's `extra-sandbox-paths` and registered as no input at all.
//! Run it INSIDE a recursive-nix derivation (see local/gates) to reach the
//! same daemon flag the driver runs under; outside one it exercises the
//! plain daemon socket.
use harmonia_store_content_address::ContentAddressMethodAlgorithm;
use harmonia_store_derivation::derivation::{Derivation, DerivationOutput};
use harmonia_store_derivation::derived_path::{OutputName, SingleDerivedPath};
use harmonia_store_derivation::placeholder::Placeholder;
use harmonia_store_path::StoreDir;
use nix_builder_rpc_client::{BuilderRpcClient, Patience};
use std::str::FromStr;
use std::sync::Arc;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("mode: ca | ia | drv");
    // `drv <path.drv> <output>...`: ask build_paths for ALREADY STORED
    // derivations, byte for byte the ones the driver submitted, so the only
    // variable left is this client's request path. Several outputs of one
    // drv, or several drvs, may be named as `path^output` pairs.
    if mode == "drv" {
        let store_dir = StoreDir::new(std::path::Path::new("/nix/store")).unwrap();
        let pool: usize = std::env::var("NN_POOL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);
        let client = Arc::new(BuilderRpcClient::connect_from_env(Some(pool)).expect("connect"));
        let want: Vec<SingleDerivedPath> = args
            .map(|spec| {
                let (drv, output) = spec.split_once('^').unwrap_or((spec.as_str(), "out"));
                SingleDerivedPath::Built {
                    drv_path: Arc::new(SingleDerivedPath::Opaque(store_dir.parse(drv).unwrap())),
                    output: OutputName::from_str(output).unwrap(),
                }
            })
            .collect();
        eprintln!("asking for {} derived path(s)", want.len());
        // NN_PRIME_CONNECTIONS=N opens N extra pool connections FIRST, by
        // issuing N cheap concurrent adds, so the build request lands on a
        // daemon that already holds several workers for this pid: the
        // shape the driver produces (the crashing run shows three accepts
        // from one pid within 5 ms, then every worker dead).
        // NN_PRIME_CONNECTIONS applies to the ca/ia arms too, below.
        if let Ok(n) = std::env::var("NN_PRIME_CONNECTIONS") {
            let n: usize = n.parse().unwrap_or(0);
            std::thread::scope(|sc| {
                for i in 0..n {
                    let c = client.clone();
                    sc.spawn(move || {
                        let _ = c.add_to_store_text(
                            &format!("nn-prime-{i}"),
                            format!("prime {i}\n").as_bytes(),
                        );
                    });
                }
            });
            eprintln!("primed {n} connection(s)");
        }
        match client.build_paths(&store_dir, &want, Patience::Single) {
            Ok(paths) => println!("UNEXPECTED SUCCESS: {paths:?}"),
            Err(e) => println!("build_paths returned an error, as it should: {e}"),
        }
        return;
    }
    let bash = args.next().expect("a bash store path");
    let builder_override = args.next();
    let store_dir = StoreDir::new(std::path::Path::new("/nix/store")).unwrap();
    let pool: usize = std::env::var("NN_POOL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let client = Arc::new(BuilderRpcClient::connect_from_env(Some(pool)).expect("connect"));

    let system = std::env::var("NIX_NINJA_SYSTEM").unwrap_or_else(|_| "x86_64-linux".into());
    let builder = builder_override
        .clone()
        .unwrap_or_else(|| format!("{bash}/bin/bash"));
    let mut drv = Derivation::new(
        "nn-failing-build".parse().unwrap(),
        system.into_bytes().into(),
        builder.clone().into_bytes().into(),
    );
    drv.args.push(b"-c"[..].into());
    // NN_SALT makes the derivation distinct so two failing builds can share
    // one request without one being a cache hit for the other.
    let salt = std::env::var("NN_SALT").unwrap_or_default();
    drv.args.push(
        format!("echo about to fail {salt}; exit 7")
            .into_bytes()
            .into(),
    );
    if builder_override.is_none() {
        drv.inputs
            .insert(SingleDerivedPath::Opaque(store_dir.parse(&bash).unwrap()));
    }
    eprintln!("builder {builder}, inputs {}", drv.inputs.len());
    let out = OutputName::from_str("out").unwrap();
    match mode.as_str() {
        "ca" => {
            drv.outputs.insert(
                out.clone(),
                DerivationOutput::CAFloating(ContentAddressMethodAlgorithm::Text),
            );
            drv.env.insert(
                b"out"[..].into(),
                Placeholder::standard_output(&out)
                    .render()
                    .into_os_string()
                    .into_encoded_bytes()
                    .into(),
            );
        }
        "ia" => {
            // Deferred lets the daemon compute the input-addressed path.
            drv.outputs.insert(out.clone(), DerivationOutput::Deferred);
        }
        other => panic!("unknown mode {other}"),
    }

    let drv_path = client
        .add_drv_to_store(&store_dir, &drv)
        .expect("add_drv_to_store");
    eprintln!("submitted {}", store_dir.display(&drv_path));
    if std::env::var_os("NN_MINT_ONLY").is_some() {
        println!("{}", store_dir.display(&drv_path));
        return;
    }
    if let Ok(n) = std::env::var("NN_PRIME_CONNECTIONS") {
        let n: usize = n.parse().unwrap_or(0);
        std::thread::scope(|sc| {
            for i in 0..n {
                let c = client.clone();
                sc.spawn(move || {
                    let _ = c.add_to_store_text(
                        &format!("nn-prime-{i}"),
                        format!("prime {i}\n").as_bytes(),
                    );
                });
            }
        });
        eprintln!("primed {n} connection(s)");
    }
    let want = SingleDerivedPath::Built {
        drv_path: Arc::new(SingleDerivedPath::Opaque(drv_path)),
        output: out,
    };
    match client.build_paths(&store_dir, std::slice::from_ref(&want), Patience::Single) {
        Ok(paths) => println!("UNEXPECTED SUCCESS: {paths:?}"),
        Err(e) => println!("build_paths returned an error, as it should: {e}"),
    }
}
