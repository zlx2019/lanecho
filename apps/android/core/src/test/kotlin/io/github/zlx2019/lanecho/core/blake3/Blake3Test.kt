package io.github.zlx2019.lanecho.core.blake3

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class Blake3Test {

    // The full official vector suite (35 cases, extended 131-byte outputs for
    // hash/keyed_hash/derive_key). Passing it is the transitive proof of
    // byte-for-byte agreement with the Rust `blake3` crate, which pins the
    // wire fingerprint format
    @Test
    fun officialVectors() {
        val text = checkNotNull(javaClass.getResourceAsStream("/blake3_test_vectors.json")) {
            "vectors resource missing"
        }.readBytes().decodeToString()
        val doc = Json.parseToJsonElement(text).jsonObject
        val key = doc.getValue("key").jsonPrimitive.content.encodeToByteArray()
        val context = doc.getValue("context_string").jsonPrimitive.content
        val cases = doc.getValue("cases").jsonArray
        assertEquals(35, cases.size)

        for (case in cases) {
            val obj = case.jsonObject
            val inputLen = obj.getValue("input_len").jsonPrimitive.int
            val input = ByteArray(inputLen) { (it % 251).toByte() }
            val expectHash = obj.getValue("hash").jsonPrimitive.content
            val expectKeyed = obj.getValue("keyed_hash").jsonPrimitive.content
            val expectDerive = obj.getValue("derive_key").jsonPrimitive.content
            val extLen = expectHash.length / 2

            assertEquals(expectHash, Blake3.newHasher().update(input).finalize(extLen).toHex(), "hash len=$inputLen")
            assertEquals(expectKeyed, Blake3.newKeyed(key).update(input).finalize(extLen).toHex(), "keyed len=$inputLen")
            assertEquals(
                expectDerive,
                Blake3.newDeriveKey(context).update(input).finalize(extLen).toHex(),
                "derive len=$inputLen",
            )
            // The 32-byte one-shot API is the prefix of the extended output
            assertEquals(expectHash.substring(0, 64), Blake3.hashHex(input), "prefix len=$inputLen")
        }
    }

    // Split updates at awkward boundaries must agree with the one-shot hash
    @Test
    fun incrementalUpdatesMatchOneShot() {
        val input = ByteArray(10_000) { (it % 251).toByte() }
        val oneShot = Blake3.hashHex(input)
        for (step in intArrayOf(1, 7, 63, 64, 65, 1023, 1024, 1025, 4096)) {
            val hasher = Blake3.newHasher()
            var pos = 0
            while (pos < input.size) {
                val take = minOf(step, input.size - pos)
                hasher.update(input, pos, pos + take)
                pos += take
            }
            assertEquals(oneShot, hasher.finalize().toHex(), "step=$step")
        }
    }

    @Test
    fun hexIs64LowercaseChars() {
        val hex = Blake3.hashHex("lanecho".encodeToByteArray())
        assertEquals(64, hex.length)
        assertTrue(hex.all { it in "0123456789abcdef" })
    }
}
