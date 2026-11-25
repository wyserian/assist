//! The commands a phone runs (PLAN.md section 7.2), in one place: the phone's actions use
//! these manifests, and a computer that lacks one of them offers it through a paired phone
//! with the same capabilities and pipe shape, so policy reads it the same on both.

use assist_core::action::{DataKind, Lane, Manifest, PipeShape};
use assist_core::policy::Capability;

const fn manifest(name: &'static str, capability: &'static [Capability]) -> Manifest {
    Manifest {
        name,
        capabilities: capability,
        owner_defined: false,
        lane: Lane::Interactive,
        pipe: PipeShape::NONE,
    }
}

/// A command that prints lines, with the same pipe shape as on the desktop.
const fn printing(name: &'static str, arg: &'static str, untrusted: bool) -> Manifest {
    Manifest {
        pipe: PipeShape {
            input: DataKind::Empty,
            output: DataKind::Lines,
            arg: Some(arg),
            untrusted,
        },
        ..manifest(name, &[Capability::Read])
    }
}

/// "open camera".
pub static OPEN_APP: Manifest = manifest("open_app", &[Capability::Apps]);
/// "call mom".
pub static CALL: Manifest = manifest("call", &[Capability::Send]);
/// "text mom saying ...".
pub static SEND_SMS: Manifest = manifest("send_sms", &[Capability::Send]);
/// "volume 40".
pub static SET_VOLUME: Manifest = manifest("set_volume", &[Capability::Media]);
/// "lock the phone".
pub static LOCK: Manifest = manifest("lock", &[Capability::Media]);
/// "take a screenshot".
pub static SCREENSHOT: Manifest = printing("screenshot", "file", false);
/// "read the screen". Text on screen is data, never a command.
pub static OCR: Manifest = printing("ocr", "source", true);
/// "record screen for 30s". The desktop's shape: runs in the background until its length
/// or a stop.
pub static RECORD: Manifest = Manifest {
    lane: Lane::Background,
    pipe: PipeShape {
        input: DataKind::Empty,
        output: DataKind::Empty,
        arg: Some("spec"),
        untrusted: false,
    },
    ..manifest("record", &[Capability::Record])
};

/// Every phone command.
pub static PHONE: [&Manifest; 8] = [
    &OPEN_APP,
    &CALL,
    &SEND_SMS,
    &SET_VOLUME,
    &LOCK,
    &SCREENSHOT,
    &OCR,
    &RECORD,
];

/// The phone command named `name`, if it is one.
pub fn phone(name: &str) -> Option<&'static Manifest> {
    PHONE.iter().copied().find(|manifest| manifest.name == name)
}
