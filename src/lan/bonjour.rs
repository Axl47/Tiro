//! The listener's Bonjour advert, so a paired phone finds this Mac again when
//! its address changes: `_tiro._tcp` on the LAN port under the computer's
//! name, with the certificate's fingerprint in the TXT record. The phone
//! still pins that certificate, so the advert only says where to look; it
//! can't make a phone trust anything.
//!
//! Registered with the system's mDNSResponder (`dns_sd.h`, in libSystem), so
//! Tiro answers no multicast itself. Dropping the advert withdraws it.

use std::ffi::{CString, c_char, c_void};

const SERVICE_TYPE: &str = "_tiro._tcp";

#[repr(C)]
struct Opaque {
    _private: [u8; 0],
}

type ServiceRef = *mut Opaque;

unsafe extern "C" {
    fn DNSServiceRegister(
        service: *mut ServiceRef,
        flags: u32,
        interface_index: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        host: *const c_char,
        port_network_order: u16,
        txt_len: u16,
        txt_record: *const c_void,
        callback: Option<unsafe extern "C" fn()>,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefDeallocate(service: ServiceRef);
}

/// Advertised while it lives.
pub(super) struct Advert(ServiceRef);

impl Advert {
    /// Advertises `port` with `fingerprint`; None (logged) when mDNSResponder refuses.
    pub(super) fn new(port: u16, fingerprint: &str) -> Option<Self> {
        let regtype = CString::new(SERVICE_TYPE).ok()?;
        let txt = txt_record(&[("fp", fingerprint)])?;
        let mut service: ServiceRef = std::ptr::null_mut();
        // No name: mDNSResponder uses the computer's name and renames on a
        // conflict. No callback: registration needs none, and a failure
        // after this call only means phones can't find the Mac by name.
        let error = unsafe {
            DNSServiceRegister(
                &mut service,
                0,
                0,
                std::ptr::null(),
                regtype.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                port.to_be(),
                txt.len() as u16,
                txt.as_ptr().cast(),
                None,
                std::ptr::null_mut(),
            )
        };
        if error != 0 || service.is_null() {
            tracing::warn!(error, "LAN dictation couldn't advertise over Bonjour");
            return None;
        }
        Some(Self(service))
    }
}

impl Drop for Advert {
    fn drop(&mut self) {
        unsafe { DNSServiceRefDeallocate(self.0) };
    }
}

/// A DNS-SD TXT record: each `key=value` behind its one-byte length.
fn txt_record(pairs: &[(&str, &str)]) -> Option<Vec<u8>> {
    let mut record = Vec::new();
    for (key, value) in pairs {
        let entry = format!("{key}={value}");
        record.push(u8::try_from(entry.len()).ok()?);
        record.extend_from_slice(entry.as_bytes());
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_entries_carry_their_length() {
        assert_eq!(txt_record(&[("fp", "ab")]).unwrap(), b"\x05fp=ab");
        assert_eq!(txt_record(&[("k", &"x".repeat(300))]), None);
    }
}
