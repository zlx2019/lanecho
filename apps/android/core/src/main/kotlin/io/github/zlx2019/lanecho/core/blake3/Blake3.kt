package io.github.zlx2019.lanecho.core.blake3

// Port of the official BLAKE3 reference implementation (CC0-1.0,
// https://github.com/BLAKE3-team/BLAKE3/blob/master/reference_impl).
// Vendored on purpose: the fingerprint and every content hash must match the
// Rust `blake3` crate byte for byte, pinned by the official test vectors.
// Keyed/derive modes exist solely so the full vector suite can run.

private const val OUT_LEN = 32
private const val BLOCK_LEN = 64
private const val CHUNK_LEN = 1024

private const val CHUNK_START = 1
private const val CHUNK_END = 2
private const val PARENT = 4
private const val ROOT = 8
private const val KEYED_HASH = 16
private const val DERIVE_KEY_CONTEXT = 32
private const val DERIVE_KEY_MATERIAL = 64

private val IV = intArrayOf(
    0x6A09E667, 0xBB67AE85.toInt(), 0x3C6EF372, 0xA54FF53A.toInt(),
    0x510E527F, 0x9B05688C.toInt(), 0x1F83D9AB, 0x5BE0CD19,
)

private val MSG_PERMUTATION = intArrayOf(2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8)

private fun g(state: IntArray, a: Int, b: Int, c: Int, d: Int, mx: Int, my: Int) {
    state[a] = state[a] + state[b] + mx
    state[d] = (state[d] xor state[a]).rotateRight(16)
    state[c] = state[c] + state[d]
    state[b] = (state[b] xor state[c]).rotateRight(12)
    state[a] = state[a] + state[b] + my
    state[d] = (state[d] xor state[a]).rotateRight(8)
    state[c] = state[c] + state[d]
    state[b] = (state[b] xor state[c]).rotateRight(7)
}

private fun round(state: IntArray, m: IntArray) {
    // Mix the columns
    g(state, 0, 4, 8, 12, m[0], m[1])
    g(state, 1, 5, 9, 13, m[2], m[3])
    g(state, 2, 6, 10, 14, m[4], m[5])
    g(state, 3, 7, 11, 15, m[6], m[7])
    // Mix the diagonals
    g(state, 0, 5, 10, 15, m[8], m[9])
    g(state, 1, 6, 11, 12, m[10], m[11])
    g(state, 2, 7, 8, 13, m[12], m[13])
    g(state, 3, 4, 9, 14, m[14], m[15])
}

private fun permute(m: IntArray): IntArray {
    val permuted = IntArray(16)
    for (i in 0 until 16) permuted[i] = m[MSG_PERMUTATION[i]]
    return permuted
}

private fun compress(cv: IntArray, blockWords: IntArray, counter: Long, blockLen: Int, flags: Int): IntArray {
    val state = intArrayOf(
        cv[0], cv[1], cv[2], cv[3], cv[4], cv[5], cv[6], cv[7],
        IV[0], IV[1], IV[2], IV[3],
        counter.toInt(), (counter ushr 32).toInt(), blockLen, flags,
    )
    var block = blockWords
    round(state, block) // round 1
    block = permute(block)
    round(state, block) // round 2
    block = permute(block)
    round(state, block) // round 3
    block = permute(block)
    round(state, block) // round 4
    block = permute(block)
    round(state, block) // round 5
    block = permute(block)
    round(state, block) // round 6
    block = permute(block)
    round(state, block) // round 7
    for (i in 0 until 8) {
        state[i] = state[i] xor state[i + 8]
        state[i + 8] = state[i + 8] xor cv[i]
    }
    return state
}

private fun wordsFromLEBytes(bytes: ByteArray, wordCount: Int): IntArray {
    val words = IntArray(wordCount)
    for (i in 0 until wordCount) {
        val o = i * 4
        words[i] = (bytes[o].toInt() and 0xFF) or
            ((bytes[o + 1].toInt() and 0xFF) shl 8) or
            ((bytes[o + 2].toInt() and 0xFF) shl 16) or
            ((bytes[o + 3].toInt() and 0xFF) shl 24)
    }
    return words
}

// A finished chunk or parent node, able to yield either a chaining value or
// (for the root) an extendable stream of output bytes
private class Output(
    private val inputCV: IntArray,
    private val blockWords: IntArray,
    private val counter: Long,
    private val blockLen: Int,
    private val flags: Int,
) {
    fun chainingValue(): IntArray = compress(inputCV, blockWords, counter, blockLen, flags).copyOf(8)

    fun rootBytes(out: ByteArray) {
        var outputCounter = 0L
        var offset = 0
        while (offset < out.size) {
            val words = compress(inputCV, blockWords, outputCounter, blockLen, flags or ROOT)
            for (word in words) {
                if (offset >= out.size) return
                var w = word
                val n = minOf(4, out.size - offset)
                for (b in 0 until n) {
                    out[offset + b] = (w and 0xFF).toByte()
                    w = w ushr 8
                }
                offset += n
            }
            outputCounter++
        }
    }
}

private class ChunkState(key: IntArray, val chunkCounter: Long, private val flags: Int) {
    private var cv = key.copyOf()
    private val block = ByteArray(BLOCK_LEN)
    private var blockLen = 0
    private var blocksCompressed = 0

    fun len(): Int = blocksCompressed * BLOCK_LEN + blockLen

    private fun startFlag(): Int = if (blocksCompressed == 0) CHUNK_START else 0

    fun update(input: ByteArray, from: Int, to: Int) {
        var pos = from
        while (pos < to) {
            // If the block buffer is full, compress it and clear it. More
            // input is coming, so this compression is not CHUNK_END
            if (blockLen == BLOCK_LEN) {
                cv = compress(cv, wordsFromLEBytes(block, 16), chunkCounter, BLOCK_LEN, flags or startFlag()).copyOf(8)
                blocksCompressed++
                block.fill(0)
                blockLen = 0
            }
            val take = minOf(BLOCK_LEN - blockLen, to - pos)
            input.copyInto(block, blockLen, pos, pos + take)
            blockLen += take
            pos += take
        }
    }

    fun output(): Output =
        Output(cv, wordsFromLEBytes(block, 16), chunkCounter, blockLen, flags or startFlag() or CHUNK_END)
}

/** Incremental BLAKE3 hasher (hash mode by default). */
class Blake3 private constructor(private val keyWords: IntArray, private val flags: Int) {
    private var chunkState = ChunkState(keyWords, 0, flags)
    // Space for 54 subtree chaining values: 2^54 * CHUNK_LEN = 2^64
    private val cvStack = Array(54) { IntArray(0) }
    private var cvStackLen = 0

    companion object {
        fun newHasher(): Blake3 = Blake3(IV.copyOf(), 0)

        fun newKeyed(key: ByteArray): Blake3 {
            require(key.size == OUT_LEN) { "key must be 32 bytes" }
            return Blake3(wordsFromLEBytes(key, 8), KEYED_HASH)
        }

        fun newDeriveKey(context: String): Blake3 {
            val contextHasher = Blake3(IV.copyOf(), DERIVE_KEY_CONTEXT)
            contextHasher.update(context.encodeToByteArray())
            val contextKey = contextHasher.finalize(OUT_LEN)
            return Blake3(wordsFromLEBytes(contextKey, 8), DERIVE_KEY_MATERIAL)
        }

        /** One-shot 32-byte hash. */
        fun hash(input: ByteArray): ByteArray = newHasher().update(input).finalize(OUT_LEN)

        /** One-shot hash as 64 lowercase hex characters (the fingerprint form). */
        fun hashHex(input: ByteArray): String = hash(input).toHex()
    }

    private fun pushStack(cv: IntArray) {
        cvStack[cvStackLen] = cv
        cvStackLen++
    }

    private fun popStack(): IntArray {
        cvStackLen--
        return cvStack[cvStackLen]
    }

    private fun parentOutput(leftCV: IntArray, rightCV: IntArray): Output {
        val blockWords = IntArray(16)
        leftCV.copyInto(blockWords, 0)
        rightCV.copyInto(blockWords, 8)
        return Output(keyWords, blockWords, 0, BLOCK_LEN, PARENT or flags)
    }

    private fun addChunkChainingValue(newCV: IntArray, totalChunks: Long) {
        // This chunk might complete some subtrees. For each completed subtree,
        // its left child will be the current top entry in the CV stack, and
        // its right child will be the current value of newCV. Pop each left
        // child off the stack, merge it with newCV, and overwrite newCV.
        // The number of completed subtrees is given by the number of trailing
        // zero bits in the new total number of chunks
        var cv = newCV
        var total = totalChunks
        while (total and 1L == 0L) {
            cv = parentOutput(popStack(), cv).chainingValue()
            total = total shr 1
        }
        pushStack(cv)
    }

    fun update(input: ByteArray, from: Int = 0, to: Int = input.size): Blake3 {
        var pos = from
        while (pos < to) {
            // If the current chunk is complete, finalize it and reset the
            // chunk state. More input is coming, so this chunk is not ROOT
            if (chunkState.len() == CHUNK_LEN) {
                val chunkCV = chunkState.output().chainingValue()
                val totalChunks = chunkState.chunkCounter + 1
                addChunkChainingValue(chunkCV, totalChunks)
                chunkState = ChunkState(keyWords, totalChunks, flags)
            }
            val want = CHUNK_LEN - chunkState.len()
            val take = minOf(want, to - pos)
            chunkState.update(input, pos, pos + take)
            pos += take
        }
        return this
    }

    fun finalize(outLen: Int = OUT_LEN): ByteArray {
        // Starting with the Output from the current chunk, compute all the
        // parent chaining values along the right edge of the tree
        var output = chunkState.output()
        var parentsRemaining = cvStackLen
        while (parentsRemaining > 0) {
            parentsRemaining--
            output = parentOutput(cvStack[parentsRemaining], output.chainingValue())
        }
        val out = ByteArray(outLen)
        output.rootBytes(out)
        return out
    }
}

private const val HEX_CHARS = "0123456789abcdef"

/** Lowercase hex encoding (fingerprints, content hashes, blob names). */
fun ByteArray.toHex(): String {
    val sb = StringBuilder(size * 2)
    for (b in this) {
        val v = b.toInt() and 0xFF
        sb.append(HEX_CHARS[v ushr 4]).append(HEX_CHARS[v and 0xF])
    }
    return sb.toString()
}
