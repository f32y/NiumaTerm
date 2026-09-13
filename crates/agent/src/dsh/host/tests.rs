use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use crate::LaunchConfig;
use crate::dsh::host::{HostError, host_slot, shared};

#[test]
fn a_starting_launch_does_not_block_another_launch() {
    let launch = LaunchConfig {
        executable: "nmt-test-starting-dsh-host".into(),
        ..LaunchConfig::default()
    };

    let slot = host_slot(&launch);

    assert!(Arc::ptr_eq(&slot, &host_slot(&launch)));

    let starting = slot.lock();
    let (completed, received) = mpsc::channel();

    let worker = thread::spawn(move || {
        let different_launch = LaunchConfig {
            executable: "nmt-test-missing-dsh-host".into(),
            ..LaunchConfig::default()
        };

        let result = shared(&different_launch);

        completed
            .send(matches!(result, Err(HostError::NotInstalled(_))))
            .unwrap();
    });

    let result = received.recv_timeout(Duration::from_secs(3));

    drop(starting);
    worker.join().unwrap();

    assert!(result.unwrap());
    assert!(Arc::ptr_eq(&slot, &host_slot(&launch)));
}
