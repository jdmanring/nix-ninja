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
    let mode = args.next().expect("mode: ca | ia | drv | serial");
    if mode == "serial" {
        serial(args);
        return;
    }
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

/// `serial <bash-store-path> <N> <K> <S>`: the daemon worker growth fixture
/// (local/pending/daemon-worker-growth-fixture.md). Adds K text sources and
/// mints N task derivations shaped like the driver's (floating CA NAR
/// outputs, each task naming every source in its input sources, as a
/// layer-1 task names its inputs, and carrying S bytes of `passAsFile`
/// padding), then realises each task in a `build_paths` of its own and
/// prints `realised <i> <unix-ms>`.
///
/// Two clients, so one worker serves the series and nothing else. Minting
/// goes through a host-route client, which uploads with the plain add
/// operation every daemon speaks, and is dropped before the series, so its
/// worker exits. The series client is an in-build one with a pool of one:
/// its pool carries no count or byte quota, so the worker is never
/// retired, and `build_paths` sends the same request on either route.
///
/// NN_SOCKET names the daemon socket. NN_SALT makes the derivations
/// distinct from an earlier run's. NN_DRVS_OUT writes the minted task
/// derivations, one per line; NN_DRVS reads such a list and realises it
/// instead of minting (the repeat arm). NN_FRESH_SOURCES=F makes F of each
/// task's K sources its own, the rest shared by every task: a growth kept
/// per distinct store path reads flat when every task shares one set.
/// NN_SERIES_HOST makes the series client a host-route one, with its count
/// and byte quotas, so its worker is retired as the driver's are; the run
/// ends `recycled <n>`, the retirements this process made. The byte charge
/// reads each derivation from /nix/store, so the client must see the store
/// the daemon writes.
fn serial(mut args: impl Iterator<Item = String>) {
    use harmonia_store_path::StorePath;
    let bash = args.next().expect("a bash store path");
    let n: usize = args.next().expect("N").parse().expect("N");
    let k: usize = args.next().expect("K").parse().expect("K");
    let s: usize = args.next().expect("S").parse().expect("S");
    let store_dir = StoreDir::new(std::path::Path::new("/nix/store")).unwrap();
    let socket =
        std::env::var("NN_SOCKET").unwrap_or_else(|_| "/nix/var/nix/daemon-socket/socket".into());
    let socket = std::path::Path::new(&socket);
    let salt = std::env::var("NN_SALT").unwrap_or_default();
    let out = OutputName::from_str("out").unwrap();
    let built = |p: &StorePath| SingleDerivedPath::Built {
        drv_path: Arc::new(SingleDerivedPath::Opaque(p.clone())),
        output: out.clone(),
    };
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    };

    let tasks: Vec<StorePath> = match std::env::var("NN_DRVS") {
        Ok(list) => std::fs::read_to_string(&list)
            .expect("NN_DRVS")
            .lines()
            .map(|l| store_dir.parse(l).expect("a store path per line"))
            .collect(),
        Err(_) => {
            let minter = BuilderRpcClient::connect_unix_sized(socket, false, 1).expect("connect");
            let mint = |name: String, script: String, inputs: &[SingleDerivedPath], pad: usize| {
                let mut drv = Derivation::new(
                    name.parse().unwrap(),
                    b"x86_64-linux"[..].into(),
                    format!("{bash}/bin/bash").into_bytes().into(),
                );
                drv.args.push(b"-c"[..].into());
                drv.args.push(script.into_bytes().into());
                drv.inputs
                    .insert(SingleDerivedPath::Opaque(store_dir.parse(&bash).unwrap()));
                drv.inputs.extend(inputs.iter().cloned());
                if pad > 0 {
                    drv.env.insert(b"PAD"[..].into(), vec![b'x'; pad].into());
                    drv.env.insert(b"passAsFile"[..].into(), b"PAD"[..].into());
                }
                drv.outputs.insert(
                    out.clone(),
                    DerivationOutput::CAFloating(ContentAddressMethodAlgorithm::NixArchive(
                        harmonia_utils_hash::Algorithm::SHA256,
                    )),
                );
                drv.env.insert(
                    b"out"[..].into(),
                    Placeholder::standard_output(&out)
                        .render()
                        .into_os_string()
                        .into_encoded_bytes()
                        .into(),
                );
                minter
                    .add_drv_to_store(&store_dir, &drv)
                    .expect("add_drv_to_store")
            };
            let fresh: usize = std::env::var("NN_FRESH_SOURCES")
                .map(|v| v.parse().expect("NN_FRESH_SOURCES"))
                .unwrap_or(0);
            assert!(fresh <= k, "NN_FRESH_SOURCES exceeds K");
            let add = |name: String| {
                let p = minter
                    .add_to_store_text(&name, format!("{name} {salt}\n").as_bytes())
                    .expect("add a source");
                SingleDerivedPath::Opaque(p)
            };
            let shared: Vec<SingleDerivedPath> =
                (0..k - fresh).map(|j| add(format!("nn-src-{j}"))).collect();
            eprintln!("added {} shared source(s)", k - fresh);
            let tasks: Vec<StorePath> = (0..n)
                .map(|i| {
                    let mut sources = shared.clone();
                    sources.extend((0..fresh).map(|j| add(format!("nn-src-{i}-{j}"))));
                    mint(
                        format!("nn-task-{i}"),
                        format!("echo task {i} {salt} > $out"),
                        &sources,
                        s,
                    )
                })
                .collect();
            eprintln!("added {fresh} fresh source(s) per task");
            if let Ok(path) = std::env::var("NN_DRVS_OUT") {
                let list: Vec<String> = tasks
                    .iter()
                    .map(|t| store_dir.display(t).to_string())
                    .collect();
                std::fs::write(path, list.join("\n") + "\n").expect("NN_DRVS_OUT");
            }
            tasks
        }
    };

    if let Ok(passes) = std::env::var("NN_PASSES") {
        raw_passes(
            socket,
            &store_dir,
            &tasks,
            passes.parse().expect("NN_PASSES"),
        );
        return;
    }
    let host = std::env::var_os("NN_SERIES_HOST").is_some();
    let series = BuilderRpcClient::connect_unix_sized(socket, !host, 1).expect("connect");
    println!("series start {} task(s) {}", tasks.len(), now_ms());
    for (i, t) in tasks.iter().enumerate() {
        series
            .build_paths(&store_dir, &[built(t)], Patience::Single)
            .expect("realise");
        println!("realised {} {}", i + 1, now_ms());
    }
    let recycled = nix_builder_rpc_client::RECYCLED.load(std::sync::atomic::Ordering::Relaxed);
    println!("recycled {recycled}");
}

/// NN_PASSES=P: realise TASKS P times over, on ONE raw daemon connection.
/// The crate's client memoises realises per process, so its second pass
/// would send nothing; a raw harmonia client sends the same
/// BuildPathsWithResults request every time. Realise lines are numbered
/// across passes (pass 2 of 100 tasks is 101..200).
fn raw_passes(
    socket: &std::path::Path,
    store_dir: &StoreDir,
    tasks: &[harmonia_store_path::StorePath],
    passes: usize,
) {
    use harmonia_protocol::daemon_wire::types2::BuildMode;
    use harmonia_store_derivation::derived_path::{DerivedPath, OutputSpec};
    use harmonia_store_remote::{DaemonClientBuilder, DaemonStore, HandshakeDaemonStore};
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    };
    rt.block_on(async {
        let mut client = DaemonClientBuilder::new()
            .set_store_dir(store_dir)
            .build_unix(socket)
            .await
            .expect("connect")
            .handshake()
            .await
            .expect("handshake");
        // NN_PAUSE_MS: idle after each realise so a 1 s sampler sees every
        // one; an already-valid realise takes milliseconds.
        let pause: u64 = std::env::var("NN_PAUSE_MS")
            .map(|v| v.parse().expect("NN_PAUSE_MS"))
            .unwrap_or(0);
        println!("series start {} task(s) {}", tasks.len() * passes, now_ms());
        let mut i = 0;
        for _ in 0..passes {
            for t in tasks {
                let want = [DerivedPath::Built {
                    drv_path: Arc::new(SingleDerivedPath::Opaque(t.clone())),
                    outputs: OutputSpec::Named(
                        std::iter::once(OutputName::from_str("out").unwrap()).collect(),
                    ),
                }];
                let res = client
                    .build_paths_with_results(&want, BuildMode::Normal)
                    .await
                    .expect("realise");
                assert_eq!(res.len(), 1, "one result per request");
                if let harmonia_protocol::daemon_wire::types2::BuildResultInner::Failure(f) =
                    &res[0].result.inner
                {
                    panic!("realise {} failed: {f:?}", i + 1);
                }
                i += 1;
                println!("realised {i} {}", now_ms());
                if pause > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(pause)).await;
                }
            }
        }
    });
}
