use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::ErrorKind;
use std::mem;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU32, Ordering};

const RTM_NEWLINK: u16 = 16;
const RTM_DELLINK: u16 = 17;
const NLM_F_REQUEST: u16 = 0x01;
const NLM_F_ACK: u16 = 0x04;
const NLM_F_EXCL: u16 = 0x200;
const NLM_F_CREATE: u16 = 0x400;
const NLMSG_ERROR: u16 = 2;
const NLMSG_HDRLEN: usize = 16;
const IFLA_ADDRESS: u16 = 1;
const IFLA_IFNAME: u16 = 3;
const IFLA_LINK: u16 = 5;
const IFLA_MASTER: u16 = 10;
const IFLA_LINKINFO: u16 = 18;
const IFLA_INFO_KIND: u16 = 1;
const IFLA_INFO_DATA: u16 = 2;
const IFLA_MACVLAN_MODE: u16 = 1;
const MACVLAN_MODE_BRIDGE: u32 = 4;
const IF_FLAG_UP: u32 = 1;
const SIOCETHTOOL: libc::Ioctl = 0x8946 as libc::Ioctl;
const ETHTOOL_GTXCSUM: u32 = 0x16;
const ETHTOOL_STXCSUM: u32 = 0x17;
const ETHTOOL_STSO: u32 = 0x1f;
const ETHTOOL_SGSO: u32 = 0x24;
const ETHTOOL_SGRO: u32 = 0x2c;

#[repr(C)]
struct EthtoolValue {
    cmd: u32,
    data: u32,
}

static NL_SEQ: AtomicU32 = AtomicU32::new(1);

struct Netlink {
    fd: i32,
}

impl Netlink {
    fn open() -> Result<Self> {
        // SAFETY: socket() with a constant domain and type. A negative return is an error.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("Failed to open a netlink socket");
        }
        let sock = Self { fd };
        // SAFETY: sockaddr_nl is a plain C struct. Zeroed is a valid unbound address.
        let mut local: libc::sockaddr_nl = unsafe { mem::zeroed() };
        local.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // SAFETY: bind consumes the address by value. The fd is a live netlink socket.
        let rc = unsafe {
            libc::bind(
                sock.fd,
                &local as *const libc::sockaddr_nl as *const libc::sockaddr,
                mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error())
                .context("Failed to bind the netlink socket");
        }
        let timeout = libc::timeval {
            tv_sec: 2,
            tv_usec: 0,
        };
        // SAFETY: SO_RCVTIMEO takes a timeval whose lifetime covers this call.
        let rc = unsafe {
            libc::setsockopt(
                sock.fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &timeout as *const libc::timeval as *const libc::c_void,
                mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error()).context("Failed to set netlink timeout");
        }
        Ok(sock)
    }

    fn send(&self, buf: &[u8]) -> Result<()> {
        // SAFETY: destination is an all-zero sockaddr_nl, which addresses the kernel.
        let mut dest: libc::sockaddr_nl = unsafe { mem::zeroed() };
        dest.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        let rc = unsafe {
            libc::sendto(
                self.fd,
                buf.as_ptr() as *const libc::c_void,
                buf.len(),
                0,
                &dest as *const libc::sockaddr_nl as *const libc::sockaddr,
                mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error())
                .context("Failed to send a netlink message");
        }
        Ok(())
    }

    fn recv_ack(&self) -> Result<()> {
        let mut buf = vec![0u8; 8192];
        for _ in 0..8 {
            // SAFETY: buf is writable for its full length. The fd is a live socket.
            let n =
                unsafe { libc::recv(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(err).context("Failed to read a netlink reply");
            }
            if let Some(result) = parse_ack(&buf[..n as usize])? {
                return result;
            }
        }
        Err(anyhow!("Netlink reply did not include an acknowledgement"))
    }
}

impl Drop for Netlink {
    fn drop(&mut self) {
        // SAFETY: fd came from socket() and is closed once.
        unsafe { libc::close(self.fd) };
    }
}

struct OwnedFd(i32);

impl Drop for OwnedFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            // SAFETY: fd came from open() and is closed once.
            unsafe { libc::close(self.0) };
        }
    }
}

pub fn create_macvtap(ifname: &str, parent: &str, mac: Option<&str>) -> Result<()> {
    let parent_index = ifindex(parent)?;
    let mac_bytes = match mac {
        Some(text) if !text.trim().is_empty() => Some(parse_mac(text)?),
        _ => None,
    };
    let msg = macvtap_newlink(ifname, parent_index, mac_bytes.as_ref(), next_seq());
    talk(&msg).with_context(|| format!("Failed to create macvtap device '{ifname}'"))
}

pub fn create_tap(ifname: &str) -> Result<()> {
    let path = std::ffi::CString::new("/dev/net/tun").expect("path has no NUL");
    // SAFETY: open() with a NUL-terminated path. A negative return is an error.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("Failed to open /dev/net/tun");
    }
    let _owned = OwnedFd(fd);
    // SAFETY: ifreq is a plain C struct. Zeroed is the empty name the ioctl expects.
    let mut req: libc::ifreq = unsafe { mem::zeroed() };
    fill_ifr_name(&mut req, ifname)?;
    req.ifr_ifru.ifru_flags = (libc::IFF_TAP | libc::IFF_NO_PI) as libc::c_short;
    // SAFETY: TUNSETIFF reads the ifreq for the lifetime of the call. fd is open.
    let rc = unsafe { libc::ioctl(fd, libc::TUNSETIFF, &mut req) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("Failed to create tap device '{ifname}'"));
    }
    // TUNSETPERSIST takes the integer directly. The device then survives close().
    // SAFETY: fd is the tap descriptor returned by TUNSETIFF.
    let rc = unsafe { libc::ioctl(fd, libc::TUNSETPERSIST, 1i32) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("Failed to persist tap device '{ifname}'"));
    }
    Ok(())
}

pub fn set_master(ifname: &str, master: &str) -> Result<()> {
    let msg = link_modify(ifindex(ifname)?, Some(ifindex(master)?), false, next_seq());
    talk(&msg).with_context(|| format!("Failed to attach '{ifname}' to bridge '{master}'"))
}

pub fn set_up(ifname: &str) -> Result<()> {
    let msg = link_modify(ifindex(ifname)?, None, true, next_seq());
    talk(&msg).with_context(|| format!("Failed to bring up interface '{ifname}'"))
}

/// Same as `ethtool -K <tap> tx off`. TAP TX checksum offload leaves UDP
/// checksums incomplete (CHECKSUM_PARTIAL); the guest then drops replies
/// from host services on the bridge. TSO/GSO follow TX checksum; GRO is
/// independent and is turned off to match the working host TAP.
pub fn disable_tap_tx_offload(ifname: &str) -> Result<()> {
    ethtool_value(ifname, ETHTOOL_STXCSUM, 0)
        .with_context(|| format!("Failed to disable TX checksum offload on '{ifname}'"))?;
    let _ = ethtool_value(ifname, ETHTOOL_STSO, 0);
    let _ = ethtool_value(ifname, ETHTOOL_SGSO, 0);
    let _ = ethtool_value(ifname, ETHTOOL_SGRO, 0);
    Ok(())
}

pub fn tx_checksum_enabled(ifname: &str) -> Result<bool> {
    Ok(ethtool_value(ifname, ETHTOOL_GTXCSUM, 0)? != 0)
}

fn ethtool_value(ifname: &str, cmd: u32, data: u32) -> Result<u32> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("Failed to open an ethtool socket");
    }
    let _owned = OwnedFd(fd);
    let mut eval = EthtoolValue { cmd, data };
    // SAFETY: ifreq is a plain C struct. Zeroed name is filled next.
    let mut req: libc::ifreq = unsafe { mem::zeroed() };
    fill_ifr_name(&mut req, ifname)?;
    req.ifr_ifru.ifru_data = (&mut eval as *mut EthtoolValue).cast();
    // SAFETY: fd is a live socket; ifr_data points at eval for this call.
    let rc = unsafe { libc::ioctl(fd, SIOCETHTOOL, &mut req) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("Failed to set ethtool command 0x{cmd:x} on '{ifname}'"));
    }
    Ok(eval.data)
}

fn fill_ifr_name(req: &mut libc::ifreq, ifname: &str) -> Result<()> {
    let bytes = ifname.as_bytes();
    if bytes.len() >= libc::IFNAMSIZ {
        return Err(anyhow!(
            "Interface name '{ifname}' does not fit in IFNAMSIZ"
        ));
    }
    for (idx, byte) in bytes.iter().enumerate() {
        req.ifr_name[idx] = *byte as c_char;
    }
    Ok(())
}

pub fn delete_link(ifname: &str) -> Result<()> {
    let msg = link_delete(ifname, next_seq());
    match talk(&msg) {
        Ok(()) => Ok(()),
        Err(err) if errno_of(&err) == Some(libc::ENODEV) => Ok(()),
        Err(err) => Err(err).with_context(|| format!("Failed to delete interface '{ifname}'")),
    }
}

fn talk(buf: &[u8]) -> Result<()> {
    let sock = Netlink::open()?;
    sock.send(buf)?;
    sock.recv_ack()
}

fn ifindex(name: &str) -> Result<i32> {
    let path = format!("/sys/class/net/{name}/ifindex");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("Network device '{name}' was not found"))?;
    text.trim()
        .parse::<i32>()
        .with_context(|| format!("Invalid ifindex for '{name}'"))
}

fn parse_mac(mac: &str) -> Result<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = mac.split(':');
    for slot in &mut out {
        let part = parts
            .next()
            .ok_or_else(|| anyhow!("MAC '{mac}' must look like 52:54:00:00:00:01"))?;
        *slot = u8::from_str_radix(part, 16)
            .map_err(|_| anyhow!("MAC '{mac}' must look like 52:54:00:00:00:01"))?;
    }
    if parts.next().is_some() {
        return Err(anyhow!("MAC '{mac}' must look like 52:54:00:00:00:01"));
    }
    Ok(out)
}

fn next_seq() -> u32 {
    NL_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn errno_of(err: &anyhow::Error) -> Option<i32> {
    err.chain().find_map(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.raw_os_error())
    })
}

/// `Some(Ok)` or `Some(Err)` ends the read. `None` means this datagram had no ACK.
fn parse_ack(buf: &[u8]) -> Result<Option<Result<()>>> {
    let mut off = 0;
    while off + NLMSG_HDRLEN <= buf.len() {
        let len = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        if len < NLMSG_HDRLEN || off + len > buf.len() {
            break;
        }
        let kind = u16::from_ne_bytes(buf[off + 4..off + 6].try_into().unwrap());
        if kind == NLMSG_ERROR {
            if len < NLMSG_HDRLEN + 4 {
                return Err(anyhow!("Short netlink error message"));
            }
            let err = i32::from_ne_bytes(buf[off + 16..off + 20].try_into().unwrap());
            if err == 0 {
                return Ok(Some(Ok(())));
            }
            return Ok(Some(
                Err(std::io::Error::from_raw_os_error(-err)).context("netlink"),
            ));
        }
        off += (len + 3) & !3;
    }
    Ok(None)
}

struct Msg {
    buf: Vec<u8>,
}

impl Msg {
    fn new(kind: u16, flags: u16, seq: u32) -> Self {
        let mut buf = vec![0u8; NLMSG_HDRLEN];
        buf[4..6].copy_from_slice(&kind.to_ne_bytes());
        buf[6..8].copy_from_slice(&flags.to_ne_bytes());
        buf[8..12].copy_from_slice(&seq.to_ne_bytes());
        Self { buf }
    }

    fn ifinfo(&mut self, index: i32, flags: u32, change: u32) {
        let start = self.buf.len();
        self.buf.extend_from_slice(&[0u8; 16]);
        self.buf[start + 4..start + 8].copy_from_slice(&index.to_ne_bytes());
        self.buf[start + 8..start + 12].copy_from_slice(&flags.to_ne_bytes());
        self.buf[start + 12..start + 16].copy_from_slice(&change.to_ne_bytes());
    }

    fn align(&mut self) {
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
    }

    fn attr(&mut self, kind: u16, payload: &[u8]) {
        self.align();
        let len = (4 + payload.len()) as u16;
        self.buf.extend_from_slice(&len.to_ne_bytes());
        self.buf.extend_from_slice(&kind.to_ne_bytes());
        self.buf.extend_from_slice(payload);
        self.align();
    }

    fn begin_nest(&mut self, kind: u16) -> usize {
        self.align();
        let pos = self.buf.len();
        self.buf.extend_from_slice(&4u16.to_ne_bytes());
        self.buf.extend_from_slice(&kind.to_ne_bytes());
        pos
    }

    fn end_nest(&mut self, pos: usize) {
        self.align();
        let len = (self.buf.len() - pos) as u16;
        self.buf[pos..pos + 2].copy_from_slice(&len.to_ne_bytes());
    }

    fn finish(mut self) -> Vec<u8> {
        self.align();
        let len = self.buf.len() as u32;
        self.buf[..4].copy_from_slice(&len.to_ne_bytes());
        self.buf
    }
}

fn c_string(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

pub(crate) fn macvtap_newlink(
    ifname: &str,
    parent_index: i32,
    mac: Option<&[u8; 6]>,
    seq: u32,
) -> Vec<u8> {
    let mut msg = Msg::new(
        RTM_NEWLINK,
        NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL,
        seq,
    );
    msg.ifinfo(0, 0, 0);
    msg.attr(IFLA_IFNAME, &c_string(ifname));
    msg.attr(IFLA_LINK, &parent_index.to_ne_bytes());
    if let Some(mac) = mac {
        msg.attr(IFLA_ADDRESS, mac);
    }
    let linkinfo = msg.begin_nest(IFLA_LINKINFO);
    msg.attr(IFLA_INFO_KIND, &c_string("macvtap"));
    let data = msg.begin_nest(IFLA_INFO_DATA);
    msg.attr(IFLA_MACVLAN_MODE, &MACVLAN_MODE_BRIDGE.to_ne_bytes());
    msg.end_nest(data);
    msg.end_nest(linkinfo);
    msg.finish()
}

fn link_modify(index: i32, master: Option<i32>, up: bool, seq: u32) -> Vec<u8> {
    let mut msg = Msg::new(RTM_NEWLINK, NLM_F_REQUEST | NLM_F_ACK, seq);
    if up {
        msg.ifinfo(index, IF_FLAG_UP, IF_FLAG_UP);
    } else {
        msg.ifinfo(index, 0, 0);
    }
    if let Some(master) = master {
        msg.attr(IFLA_MASTER, &master.to_ne_bytes());
    }
    msg.finish()
}

fn link_delete(ifname: &str, seq: u32) -> Vec<u8> {
    let mut msg = Msg::new(RTM_DELLINK, NLM_F_REQUEST | NLM_F_ACK, seq);
    msg.ifinfo(0, 0, 0);
    msg.attr(IFLA_IFNAME, &c_string(ifname));
    msg.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macvtap_message_carries_kind_and_bridge_mode() {
        let mac = [0x52, 0x54, 0x00, 0x11, 0x22, 0x33];
        let msg = macvtap_newlink("mv0", 7, Some(&mac), 1);
        assert!(msg.windows(8).any(|window| window == b"macvtap\0"));
        assert!(msg.windows(4).any(|window| window == b"mv0\0"));
        assert!(msg.windows(6).any(|window| window == mac));
        assert!(msg
            .windows(8)
            .any(|window| window == [0x08, 0x00, 0x01, 0x00, 0x04, 0x00, 0x00, 0x00]));
        assert!(msg
            .windows(8)
            .any(|window| window == [0x08, 0x00, 0x05, 0x00, 0x07, 0x00, 0x00, 0x00]));
        let flags = u16::from_ne_bytes(msg[6..8].try_into().unwrap());
        assert_eq!(flags, NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL);
    }

    #[test]
    fn delete_message_names_the_device_and_ack_parser_reads_errno() {
        let msg = link_delete("qu-app-0", 3);
        assert!(msg.windows(9).any(|window| window == b"qu-app-0\0"));
        let mut ack = vec![0u8; 20];
        ack[..4].copy_from_slice(&20u32.to_ne_bytes());
        ack[4..6].copy_from_slice(&NLMSG_ERROR.to_ne_bytes());
        ack[16..20].copy_from_slice(&(-libc::ENODEV).to_ne_bytes());
        let parsed = parse_ack(&ack).unwrap().unwrap();
        let err = parsed.expect_err("enodev");
        assert_eq!(errno_of(&err), Some(libc::ENODEV));

        ack[16..20].copy_from_slice(&0i32.to_ne_bytes());
        assert!(parse_ack(&ack).unwrap().unwrap().is_ok());
    }

    #[test]
    fn ethtool_value_is_cmd_then_data() {
        assert_eq!(std::mem::size_of::<EthtoolValue>(), 8);
        assert_eq!(ETHTOOL_GTXCSUM, 0x16);
        let value = EthtoolValue {
            cmd: ETHTOOL_STXCSUM,
            data: 0,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&value as *const EthtoolValue).cast::<u8>(),
                std::mem::size_of::<EthtoolValue>(),
            )
        };
        assert_eq!(&bytes[..4], &ETHTOOL_STXCSUM.to_ne_bytes());
        assert_eq!(&bytes[4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn fill_ifr_name_rejects_overlong() {
        let mut req: libc::ifreq = unsafe { mem::zeroed() };
        assert!(fill_ifr_name(&mut req, "this-name-is-too-long").is_err());
        fill_ifr_name(&mut req, "ch-rhel").unwrap();
        let name = unsafe { std::ffi::CStr::from_ptr(req.ifr_name.as_ptr()) };
        assert_eq!(name.to_str().unwrap(), "ch-rhel");
    }
}
