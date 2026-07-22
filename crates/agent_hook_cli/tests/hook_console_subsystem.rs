use std::fs;

#[test]
fn hook_binary_is_console_subsystem() {
    let bytes = fs::read(env!("CARGO_BIN_EXE_NiumaTermHook")).unwrap();
    let pe = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    let optional_header = pe + 24;
    let subsystem = u16::from_le_bytes(
        bytes[optional_header + 68..optional_header + 70]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        subsystem, 3,
        "Hook stdin requires Windows Console subsystem"
    );
}
