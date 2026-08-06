// Transport-layer TLS loopback tests, mirroring the Rust tls.rs test surface:
// 1. TLS 1.3 mutual handshake, with both sides pulling the peer certificate
//    out of the verification callback and computing the right fingerprint
// 2. Data round trip (ping → pong)
// 3. The handshake must fail when the client pins the wrong fingerprint

import Foundation
import NIOCore
import NIOPosix
import NIOSSL
import Testing

@testable import LanechoKit

/// Minimal mutable box for crossing @Sendable callbacks; delivery is
/// concurrent, so a lock guards the value
private final class Box<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: T
    init(_ value: T) { self.value = value }
    func set(_ new: T) {
        lock.lock()
        defer { lock.unlock() }
        value = new
    }
    func get() -> T {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

/// Read a test fixture (file-private; IdentityTests keeps its own copy)
private func fixtureData(_ name: String) throws -> Data {
    let url = try #require(Bundle.module.url(forResource: "Fixtures/\(name)", withExtension: nil))
    return try Data(contentsOf: url)
}

/// Load a fixture identity
private func loadMaterial(cert: String, key: String) throws -> IdentityMaterial {
    try IdentityMaterial(
        certDER: [UInt8](fixtureData(cert)),
        keyDER: [UInt8](fixtureData(key)))
}

/// Server handler that answers "ping" with "pong"
private final class PongHandler: ChannelInboundHandler, Sendable {
    typealias InboundIn = ByteBuffer
    typealias OutboundOut = ByteBuffer

    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
        var buf = unwrapInboundIn(data)
        if buf.readString(length: buf.readableBytes) == "ping" {
            context.writeAndFlush(wrapOutboundOut(ByteBuffer(string: "pong")), promise: nil)
        }
    }
}

/// Client handler that hands the first inbound message to the promise
private final class CollectOneHandler: ChannelInboundHandler, Sendable {
    typealias InboundIn = ByteBuffer
    private let promise: EventLoopPromise<String>
    init(promise: EventLoopPromise<String>) { self.promise = promise }

    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
        var buf = unwrapInboundIn(data)
        promise.succeed(buf.readString(length: buf.readableBytes) ?? "")
    }
    func errorCaught(context: ChannelHandlerContext, error: Error) {
        promise.fail(error)
        context.close(promise: nil)
    }
}

/// Mutual authentication, both sides recognizing the peer fingerprint, and a
/// data round trip (the main path)
@Test(.timeLimit(.minutes(1)))
func tlsMutualAuthExchangesFingerprints() async throws {
    let serverMaterial = try loadMaterial(cert: "cert.der", key: "key.der")
    let clientMaterial = try loadMaterial(cert: "cert2.der", key: "key2.der")
    let serverTLS = try TLSContexts(material: serverMaterial)
    let clientTLS = try TLSContexts(material: clientMaterial)

    // Process-wide singleton event loop group: the test does not need to shut
    // it down explicitly, and cannot conveniently do so from an async context
    let group = MultiThreadedEventLoopGroup.singleton

    let seenClientFP = Box<String?>(nil)
    let seenServerFP = Box<String?>(nil)

    let server = try await ServerBootstrap(group: group)
        .childChannelInitializer { channel in
            let ssl = NIOSSLServerHandler(
                context: serverTLS.server,
                customVerificationCallback: acceptAnyCertificate { seenClientFP.set($0) })
            return channel.pipeline.addHandlers([ssl, PongHandler()])
        }
        .bind(host: "127.0.0.1", port: 0)
        .get()
    let port = try #require(server.localAddress?.port)

    let replyPromise = group.next().makePromise(of: String.self)
    let client = try await ClientBootstrap(group: group)
        .channelInitializer { channel in
            do {
                let ssl = try NIOSSLClientHandler(
                    context: clientTLS.client,
                    serverHostname: nil,
                    customVerificationCallback: pinnedCertificate(
                        expected: serverMaterial.fingerprint
                    ) { seenServerFP.set($0) })
                return channel.pipeline.addHandlers([ssl, CollectOneHandler(promise: replyPromise)])
            } catch {
                return channel.eventLoop.makeFailedFuture(error)
            }
        }
        .connect(host: "127.0.0.1", port: port)
        .get()

    try await client.writeAndFlush(ByteBuffer(string: "ping")).get()
    let reply = try await replyPromise.futureResult.get()
    #expect(reply == "pong")
    // Fingerprints match: during the handshake each side sees the BLAKE3
    // fingerprint of the peer's certificate
    #expect(seenServerFP.get() == serverMaterial.fingerprint)
    #expect(seenClientFP.get() == clientMaterial.fingerprint)

    try await client.close().get()
    try await server.close().get()
}

/// The handshake must fail when the wrong fingerprint is pinned (mirrors the
/// Rust handshake_rejects_wrong_fingerprint case)
@Test(.timeLimit(.minutes(1)))
func tlsRejectsWrongPin() async throws {
    let serverMaterial = try loadMaterial(cert: "cert.der", key: "key.der")
    let clientMaterial = try loadMaterial(cert: "cert2.der", key: "key2.der")
    let serverTLS = try TLSContexts(material: serverMaterial)
    let clientTLS = try TLSContexts(material: clientMaterial)

    // Process-wide singleton event loop group: the test does not need to shut
    // it down explicitly, and cannot conveniently do so from an async context
    let group = MultiThreadedEventLoopGroup.singleton

    let server = try await ServerBootstrap(group: group)
        .childChannelInitializer { channel in
            let ssl = NIOSSLServerHandler(
                context: serverTLS.server,
                customVerificationCallback: acceptAnyCertificate { _ in })
            return channel.pipeline.addHandler(ssl)
        }
        .bind(host: "127.0.0.1", port: 0)
        .get()
    let port = try #require(server.localAddress?.port)

    // The handshake failure arrives via errorCaught; data never arrives at all
    let failure = group.next().makePromise(of: String.self)
    let wrongPin = String(repeating: "0", count: 64)
    let client = try await ClientBootstrap(group: group)
        .channelInitializer { channel in
            do {
                let ssl = try NIOSSLClientHandler(
                    context: clientTLS.client,
                    serverHostname: nil,
                    customVerificationCallback: pinnedCertificate(expected: wrongPin))
                return channel.pipeline.addHandlers([ssl, CollectOneHandler(promise: failure)])
            } catch {
                return channel.eventLoop.makeFailedFuture(error)
            }
        }
        .connect(host: "127.0.0.1", port: port)
        .get()
    _ = client  // TCP connects fine; the failure is in the TLS handshake above

    await #expect(throws: (any Error).self) {
        _ = try await failure.futureResult.get()
    }
    try? await server.close().get()
}
