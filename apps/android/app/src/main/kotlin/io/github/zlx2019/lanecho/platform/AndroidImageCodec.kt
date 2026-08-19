package io.github.zlx2019.lanecho.platform

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import io.github.zlx2019.lanecho.core.history.ImageCodec
import io.github.zlx2019.lanecho.core.history.RgbaImage
import java.nio.ByteBuffer

// BitmapFactory-backed PNG decoding. The dedup/LWW hash baseline is the
// decoded RGBA (PROTOCOL §9); pixels must be NON-premultiplied or partially
// transparent images hash differently from the desktop (the Swift premultiply
// trap: only opaque bytes agree)
object AndroidImageCodec : ImageCodec {
    override fun decodeRgba(png: ByteArray): RgbaImage? {
        val options = BitmapFactory.Options().apply {
            inPreferredConfig = Bitmap.Config.ARGB_8888
            inPremultiplied = false
        }
        val bitmap = BitmapFactory.decodeByteArray(png, 0, png.size, options) ?: return null
        return try {
            val buffer = ByteBuffer.allocate(bitmap.width * bitmap.height * 4)
            bitmap.copyPixelsToBuffer(buffer)
            RgbaImage(bitmap.width, bitmap.height, buffer.array())
        } finally {
            bitmap.recycle()
        }
    }
}
