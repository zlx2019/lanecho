// UDP multicast loopback availability probe: gates the integration tests that
// need real multicast.
//
// Why it exists: sandboxed CI machines (GitHub Actions macOS runners among
// them) send and receive unicast and mDNS fine, but multicast loopback between
// two sockets on the same host never delivers — those tests can only time out
// there, and that is not a defect in the code under test.
//
// **The probe is deliberately a standalone bare BSD socket implementation and
// never reuses DiscoveryService**: sharing one set of multicast wiring would
// make the probe fail alongside the code under test whenever that code gets
// multicast wrong, turning a real defect into an "environment unsupported"
// skip — the gate must reflect the environment only, never the code.

import Foundation
import Testing

@testable import LanechoKit

enum MulticastAvailability {
    /// Probe once per process (the environment cannot change within one swift
    /// test run); lazy initialization of a static let is itself thread-safe,
    /// so parallel cases need no extra locking
    static let isAvailable: Bool = {
        let available = probe()
        if !available { warnCoverageLoss() }
        return available
    }()

    /// Reason shown when a case is skipped
    static let skipReason: Comment =
        "UDP multicast loopback is unavailable in this environment; this case requires real multicast"

    /// A skip is lost coverage and must not go green silently: on Actions
    /// raise it to a warning annotation visible on the PR page, locally print
    /// one plain line (a skip is easy to miss in 88 lines of test output)
    private static func warnCoverageLoss() {
        let text =
            "UDP multicast loopback is unavailable; skipped discovery, end-to-end, and "
            + "reverse-direction interoperability tests that require real multicast. "
            + "These tests provided no coverage in this run."
        if ProcessInfo.processInfo.environment["GITHUB_ACTIONS"] == "true" {
            print("::warning title=Multicast tests skipped::\(text)")
        } else {
            print("[warn] \(text)")
        }
    }

    /// Bring up a receiver socket joined to the group, send from a second
    /// socket, and see whether anything arrives
    private static func probe() -> Bool {
        // Random port, to stay clear of the production port and of parallel
        // cases
        let port = UInt16.random(in: 43100...43399)
        let receiver = socket(AF_INET, SOCK_DGRAM, 0)
        guard receiver >= 0 else { return false }
        defer { close(receiver) }
        guard bindAndJoin(receiver, port: port) else { return false }

        let sender = socket(AF_INET, SOCK_DGRAM, 0)
        guard sender >= 0 else { return false }
        defer { close(sender) }
        var loop: UInt8 = 1
        setsockopt(
            sender, IPPROTO_IP, IP_MULTICAST_LOOP, &loop, socklen_t(MemoryLayout<UInt8>.size))

        // Send three times: one lost packet is not enough to call the
        // environment unavailable
        var destination = makeAddr(port: port, host: Config.multicastGroup)
        let payload = Array("lanecho-probe".utf8)
        for _ in 0..<3 {
            _ = payload.withUnsafeBytes { buffer in
                withUnsafePointer(to: &destination) { addr in
                    addr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                        sendto(
                            sender, buffer.baseAddress, buffer.count, 0, $0,
                            socklen_t(MemoryLayout<sockaddr_in>.size))
                    }
                }
            }
        }

        var scratch = [UInt8](repeating: 0, count: 64)
        let received = scratch.withUnsafeMutableBytes { recv(receiver, $0.baseAddress, $0.count, 0) }
        return received > 0
    }

    /// Bind the port, join the multicast group, and set a receive timeout so
    /// recv returns -1 instead of blocking forever
    private static func bindAndJoin(_ fd: Int32, port: UInt16) -> Bool {
        var yes: Int32 = 1
        let optLen = socklen_t(MemoryLayout<Int32>.size)
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, optLen)
        setsockopt(fd, SOL_SOCKET, SO_REUSEPORT, &yes, optLen)

        var address = makeAddr(port: port, host: nil)
        let bound = withUnsafePointer(to: &address) { addr in
            addr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0 else { return false }

        var request = ip_mreq()
        request.imr_multiaddr.s_addr = inet_addr(Config.multicastGroup)
        request.imr_interface.s_addr = INADDR_ANY
        guard
            setsockopt(
                fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, &request,
                socklen_t(MemoryLayout<ip_mreq>.size)) == 0
        else { return false }

        var timeout = timeval(tv_sec: 1, tv_usec: 500_000)
        setsockopt(
            fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        return true
    }

    /// A nil host means INADDR_ANY
    private static func makeAddr(port: UInt16, host: String?) -> sockaddr_in {
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        address.sin_addr.s_addr = host.map { inet_addr($0) } ?? INADDR_ANY
        return address
    }
}
