//! Original HID++ codec. Feature indices belong to a device session, never a model.
use anyhow::{Result, bail, ensure};

pub const CHANGE_HOST: u16 = 0x1814;
pub const REPROG: u16 = 0x1b04;
pub const SOFTWARE_ID: u8 = 0x0a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub report_id: u8,
    pub device: u8,
    pub feature: u8,
    pub address: u8,
    pub payload: Vec<u8>,
}

impl Report {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let expected = match bytes.first() {
            Some(0x10) => 7,
            Some(0x11) => 20,
            _ => bail!("Not a HID++ report"),
        };
        // Windows pads reports to the collection's declared input length.
        ensure!(
            bytes.len() >= expected && bytes[expected..].iter().all(|b| *b == 0),
            "Invalid HID++ report length"
        );
        ensure!(
            (1..=6).contains(&bytes[1]) || bytes[1] == 0xff,
            "Invalid receiver device slot"
        );
        Ok(Self {
            report_id: bytes[0],
            device: bytes[1],
            feature: bytes[2],
            address: bytes[3],
            payload: bytes[4..expected].to_vec(),
        })
    }
    pub fn notification(&self) -> bool {
        self.feature != 0xff
            && !(self.report_id == 0x10 && self.feature == 0x8f)
            && self.address & 0x0f == 0
    }
    pub fn function(&self) -> u8 {
        self.address >> 4
    }
    pub fn matches(&self, device: u8, feature: u8, address: u8) -> bool {
        self.device == device && self.feature == feature && self.address == address
    }
    pub fn error_for(&self, device: u8, feature: u8, address: u8) -> Option<u8> {
        (self.device == device
            && matches!(self.feature, 0xff | 0x8f)
            && self.address == feature
            && self.payload.first() == Some(&address))
        .then(|| self.payload.get(1).copied())
        .flatten()
    }
}

pub fn request(device: u8, feature: u8, function: u8, params: &[u8]) -> Result<[u8; 20]> {
    ensure!(
        (1..=6).contains(&device) || device == 0xff,
        "Invalid receiver slot"
    );
    ensure!(function < 16 && params.len() <= 16, "Invalid HID++ request");
    let mut out = [0; 20];
    out[..4].copy_from_slice(&[0x11, device, feature, function << 4 | SOFTWARE_ID]);
    out[4..4 + params.len()].copy_from_slice(params);
    Ok(out)
}

pub fn cid_host(cid: u16) -> Option<u8> {
    (0xd1..=0xd3).contains(&cid).then(|| (cid - 0xd1) as u8)
}

/// REPROG_CONTROLS_V4 getCidInfo: analytics is bit 2 of the *second*
/// capability byte. Bit 2 of the first byte means NONSTANDARD.
pub fn analytics_capable(info: &[u8]) -> bool {
    info.get(8).is_some_and(|flags| flags & 0x04 != 0)
}

/// Validate getCidReporting state, never a setCidReporting masked-update echo.
/// None means the reply does not contain the requested control's full state.
pub fn reporting_compatible(state: &[u8], cid: u16, analytics: bool) -> Option<bool> {
    if state.len() < if analytics { 6 } else { 5 } || state[..2] != cid.to_be_bytes() {
        return None;
    }
    let mapped = u16::from_be_bytes([state[3], state[4]]);
    let unmapped = mapped == 0 || mapped == cid;
    Some(
        unmapped
            && if analytics {
                state[5] & 1 != 0 && state[2] & 5 == 0
            } else {
                state[2] & 5 == 1
            },
    )
}

/// Interpret only the feature actually resolved on this device. Never treat replies as events.
pub fn switch_event(report: &Report, device: &crate::Device, intercepted: bool) -> Option<u8> {
    if !report.notification() || report.device != device.slot {
        return None;
    }
    let p = &report.payload;
    if report.feature == device.change_host && report.function() == 0 && p.len() >= 2 {
        return (p[0] == device.current_host && p[1] < device.host_count && p[1] != p[0])
            .then_some(p[1]);
    }
    if Some(report.feature) != device.reprog || !matches!(report.function(), 0 | 2) || p.len() < 3 {
        return None;
    }
    if report.function() == 0 && !intercepted {
        return None;
    }
    if report.function() == 2 && p[2] != 1 {
        return None;
    }
    let cid = u16::from_be_bytes([p[0], p[1]]);
    if !device.controls.iter().any(|c| c.cid == cid) {
        return None;
    }
    cid_host(cid).filter(|h| *h < device.host_count && *h != device.current_host)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reporting_capabilities_and_actual_state_are_distinct_from_update_masks() {
        assert!(!analytics_capable(&[0, 0xd1, 0, 0, 4]));
        assert!(analytics_capable(&[0, 0xd1, 0, 0, 4, 0, 0, 0, 4]));
        assert_eq!(
            reporting_compatible(&[0, 0xd1, 0, 0, 0, 1], 0xd1, true),
            Some(true)
        );
        assert_eq!(
            reporting_compatible(&[0, 0xd1, 0, 0, 0xd1, 1], 0xd1, true),
            Some(true)
        );
        assert_eq!(
            reporting_compatible(&[0, 0xd1, 0, 0, 0, 0], 0xd1, true),
            Some(false)
        );
        assert_eq!(
            reporting_compatible(&[0, 0xd1, 1, 0, 0, 1], 0xd1, true),
            Some(false)
        );
        assert_eq!(
            reporting_compatible(&[0, 0xd1, 1, 0, 0], 0xd1, false),
            Some(true)
        );
        assert_eq!(
            reporting_compatible(&[0, 0xd1, 5, 0, 0], 0xd1, false),
            Some(false)
        );
        assert_eq!(
            reporting_compatible(&[0, 0xd1, 0, 0, 0xd2, 1], 0xd1, true),
            Some(false)
        );
        assert_eq!(reporting_compatible(&[0, 0xd1, 0], 0xd1, true), None);
        assert_eq!(
            reporting_compatible(&[0, 0xd2, 0, 0, 0, 1], 0xd1, true),
            None
        );
    }
    #[test]
    fn malformed_and_padded_reports() {
        for n in 0..20 {
            assert!(Report::parse(&vec![0x11; n]).is_err());
        }
        let mut packet = request(2, 9, 0, &[]).unwrap().to_vec();
        packet.extend_from_slice(&[0; 12]);
        assert!(Report::parse(&packet).is_ok());
        packet[25] = 1;
        assert!(Report::parse(&packet).is_err());
    }
    #[test]
    fn replies_and_errors_are_addressed() {
        let p = Report::parse(&request(2, 12, 1, &[1]).unwrap()).unwrap();
        assert!(p.matches(2, 12, 0x1a));
        assert!(!p.matches(1, 12, 0x1a));
        assert!(!p.notification());
        let p = Report::parse(&[0x10, 2, 0x8f, 12, 0x1a, 7, 0]).unwrap();
        assert_eq!(p.error_for(2, 12, 0x1a), Some(7));
        assert_eq!(p.error_for(1, 12, 0x1a), None);
    }
}
