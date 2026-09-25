use super::*;
#[test]
fn authenticated_connection_rejects_replayed_handoff() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (_dir, s, _, commands) = fixture();
            let a_dir = tempfile::tempdir().unwrap();
            let b_dir = tempfile::tempdir().unwrap();
            let a = Identity::load(a_dir.path()).unwrap();
            let b = Identity::load(b_dir.path()).unwrap();
            let aid = fingerprint(&a.certificate);
            let bid = fingerprint(&b.certificate);
            let ga = Arc::new(Mutex::new(Gate {
                enabled: true,
                pins: BTreeSet::from([bid.clone()]),
                ..Default::default()
            }));
            let gb = Arc::new(Mutex::new(Gate {
                enabled: true,
                pins: BTreeSet::from([aid.clone()]),
                ..Default::default()
            }));
            let (ac, _) = a.configs(ga).unwrap();
            let (_, bs) = b.configs(gb).unwrap();
            {
                let mut state = s.lock().unwrap();
                state.id = bid;
                state.peers.clear();
                state.trust = BTreeMap::from([(
                    aid.clone(),
                    Trust {
                        id: aid.clone(),
                        name: "test".into(),
                        address: "127.0.0.1:24873".into(),
                        computer: "computer-2".into(),
                    },
                )]);
            }
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server_state = s.clone();
            let server = tokio::spawn(async move {
                let (stream, address) = listener.accept().await.unwrap();
                let tls = tokio_rustls::TlsAcceptor::from(bs)
                    .accept(stream)
                    .await
                    .unwrap();
                connection(
                    server_state,
                    tokio_rustls::TlsStream::Server(tls),
                    aid,
                    address.to_string(),
                )
                .await
            });
            let stream = TcpStream::connect(address).await.unwrap();
            let mut client = tokio_rustls::TlsConnector::from(ac)
                .connect(
                    rustls::pki_types::ServerName::try_from("easy-switch-link.local").unwrap(),
                    stream,
                )
                .await
                .unwrap();
            let mut state = s.lock().unwrap().snapshot();
            state.computer = "computer-2".into();
            write_wire(
                &mut client,
                &Wire::Hello {
                    version: 1,
                    nonce: random_id(),
                    state,
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                read_wire(&mut client).await.unwrap(),
                Wire::Hello { .. }
            ));
            let transaction = random_id();
            let prepare = Wire::Prepare {
                id: transaction,
                destination: Segment::default(),
                fraction: 0.5,
                layout: 1,
                config_generation: 1,
                computer: "computer-1".into(),
                group: "missing".into(),
                mouse: "mouse".into(),
            };
            write_wire(&mut client, &prepare).await.unwrap();
            assert!(matches!(
                read_wire(&mut client).await.unwrap(),
                Wire::Prepared { result: Err(_), .. }
            ));
            write_wire(&mut client, &prepare).await.unwrap();
            assert!(read_wire(&mut client).await.is_err());
            assert!(
                server
                    .await
                    .unwrap()
                    .unwrap_err()
                    .to_string()
                    .contains("Replayed")
            );
            assert!(commands.is_empty());
        })
        .await
        .unwrap();
    });
}
fn fixture() -> (
    tempfile::TempDir,
    State,
    mpsc::Receiver<Wire>,
    crossbeam_channel::Receiver<Dispatch>,
) {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel(32);
    let (dispatch, commands) = crossbeam_channel::bounded(8);
    let rule = Rule {
        id: "test".into(),
        enabled: true,
        source: Segment::default(),
        destination: Segment::default(),
        peer: "peer".into(),
        computer: "computer-2".into(),
        group: "desk".into(),
        mouse: "mouse".into(),
    };
    let mut config = Config::default();
    config.follow.enabled = true;
    config.follow.local_computer = "computer-1".into();
    config.follow.rules.push(rule);
    let mouse = Device {
        kind: "mouse".into(),
        id: "mouse".into(),
        receiver: "bolt".into(),
        slot: 1,
        name: "Simulated mouse".into(),
        host_count: 3,
        current_host: 0,
        change_host: 7,
        reprog: None,
        wireless_status: None,
        controls: vec![],
        intercepted: false,
        online: true,
        generation: 1,
        capability: "simulated".into(),
    };
    let peer = Peer {
        tx,
        status: PeerStatus {
            id: "peer".into(),
            computer: "computer-2".into(),
            trusted: true,
            connected: true,
            ready: true,
            generation: 1,
            config_generation: 1,
            ..Default::default()
        },
        session: 1,
        code: "123456".into(),
        ours: true,
        theirs: true,
        expires: Instant::now() + Duration::from_secs(60),
    };
    let s = Shared {
        paths: Paths::new(Some(dir.path().into())).unwrap(),
        id: "local".into(),
        gate: Arc::new(Mutex::new(Gate::default())),
        local: Local {
            config,
            devices: vec![mouse],
            paused: false,
            generation: 1,
        },
        desktop: None,
        desktop_state: DesktopState {
            displays: vec![Display {
                id: "test".into(),
                name: "test".into(),
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 800.0,
                scale: 1.0,
                binding: "test".into(),
            }],
            ready: true,
            generation: 1,
            ..Default::default()
        },
        peers: BTreeMap::from([("peer".into(), peer)]),
        trust: BTreeMap::from([(
            "peer".into(),
            Trust {
                id: "peer".into(),
                name: "test".into(),
                address: "127.0.0.1:1".into(),
                computer: "computer-2".into(),
            },
        )]),
        pending: BTreeMap::new(),
        status: Arc::new(Mutex::new(FollowStatus::default())),
        dispatch,
        outbound: false,
        connecting: BTreeSet::new(),
    };
    (dir, Arc::new(Mutex::new(s)), rx, commands)
}
#[test]
fn coordination_faults_never_dispatch_and_partial_dispatch_never_rolls_back() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        for fault in [
            "timeout",
            "permission",
            "disconnect",
            "topology",
            "config",
            "revoked",
            "lock",
            "partial",
            "success",
        ] {
            let (_dir, s, mut messages, commands) = fixture();
            let transfer = handoff(s.clone(), "test".into(), 0.5, 1, Instant::now());
            let peer = async {
                let Wire::Prepare { id, .. } = messages.recv().await.unwrap() else {
                    panic!("Expected preparation")
                };
                assert!(
                    handoff(s.clone(), "test".into(), 0.5, 1, Instant::now())
                        .await
                        .is_err()
                );
                if fault == "timeout" {
                    tokio::time::sleep(Duration::from_millis(550)).await;
                } else {
                    let reply = {
                        let mut state = s.lock().unwrap();
                        match fault {
                            "topology" => state.desktop_state.generation += 1,
                            "config" => state.local.generation += 1,
                            "revoked" => state.trust.clear(),
                            "lock" => state.desktop_state.locked = true,
                            _ => {}
                        }
                        state.pending.remove(&format!("peer:{id}")).unwrap()
                    };
                    if fault == "disconnect" {
                        drop(reply);
                    } else {
                        let _ = reply.send(if fault == "permission" {
                            Err("Permission revoked".into())
                        } else {
                            Ok(1)
                        });
                    }
                    if matches!(fault, "partial" | "success") {
                        loop {
                            if let Ok(command) = commands.try_recv() {
                                assert!(command.deadline > Instant::now());
                                let _ = command.reply.send(if fault == "partial" {
                                    Err("One HID write failed".into())
                                } else {
                                    Ok(())
                                });
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                    }
                }
                let terminal = messages.recv().await.unwrap();
                if matches!(fault, "partial" | "success") {
                    assert!(matches!(terminal, Wire::Commit { .. }));
                } else {
                    assert!(commands.is_empty());
                    assert!(matches!(terminal, Wire::Cancel { .. }));
                }
            };
            let (result, ()) = tokio::join!(transfer, peer);
            assert_eq!(result.is_ok(), fault == "success", "{fault}");
            assert!(!s.lock().unwrap().outbound);
            assert!(s.lock().unwrap().pending.is_empty());
        }
    });
}
#[test]
fn pairing_requires_both_confirmations_and_valid_deadline() {
    let (_dir, s, _, _) = fixture();
    let mut s = s.lock().unwrap();
    s.trust.clear();
    let p = s.peers.get_mut("peer").unwrap();
    p.ours = false;
    p.theirs = true;
    p.status.trusted = false;
    finish_pair(&mut s, "peer").unwrap();
    assert!(!s.trust.contains_key("peer"));
    let p = s.peers.get_mut("peer").unwrap();
    p.ours = true;
    p.expires = Instant::now();
    finish_pair(&mut s, "peer").unwrap();
    assert!(!s.trust.contains_key("peer"));
    s.peers.get_mut("peer").unwrap().expires = Instant::now() + Duration::from_secs(60);
    finish_pair(&mut s, "peer").unwrap();
    assert!(s.trust.contains_key("peer"));
    assert!(s.gate.lock().unwrap().pins.contains("peer"));
}
