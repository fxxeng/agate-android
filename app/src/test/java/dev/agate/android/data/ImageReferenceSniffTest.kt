package dev.agate.android.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * 参考图格式嗅探（`ImageOps` 里的私有 `sniff`，这里通过公开入口间接验证）。
 *
 * ── 为什么这条判据值得测 ──────────────────────────────────
 * 内核只认 PNG / JPEG / WebP（`image::guess_format` 判**文件头**，不看扩展名、
 * 也不看客户端声明的 content-type）。客户端这边如果判错，方向有两个，都难查：
 *   - 漏判（把 PNG 当成不认识的格式）：会白走一次重编码 —— 有损，而且用户
 *     上传清楚的设计稿却拿到 JPEG 质量的图；
 *   - 误判（把 HEIC 当成 JPEG）：内核回 400「参考图仅支持 PNG、JPEG、WebP」，
 *     而用户在相册里看到的那张明明是「JPG」。
 *
 * 注意：真正的解码（`BitmapFactory`）在 JVM 单测里是 stub，测不了 ——
 * 所以这里只钉「按字节认格式」这一段，而它恰好是判错代价最大的那一段。
 */
class ImageReferenceSniffTest {

    @Test
    fun png_is_recognized_by_magic_bytes() {
        val png = byteArrayOf(
            0x89.toByte(), 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0, 0, 0, 0,
        )
        assertEquals("image/png", ImageOps.sniff(png))
    }

    @Test
    fun jpeg_is_recognized_by_magic_bytes() {
        val jpeg = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 0xE0.toByte(), 0, 0, 0, 0, 0, 0, 0, 0)
        assertEquals("image/jpeg", ImageOps.sniff(jpeg))
    }

    /** WebP 要同时看 `RIFF`（0..3）与 `WEBP`（8..11），只看 RIFF 会认错别的容器。 */
    @Test
    fun webp_needs_both_riff_and_webp_markers() {
        val webp = "RIFF".toByteArray() + byteArrayOf(0, 0, 0, 0) + "WEBP".toByteArray()
        assertEquals("image/webp", ImageOps.sniff(webp))
        // 只有 RIFF 没有 WEBP（例如 WAV）：不认
        val wav = "RIFF".toByteArray() + byteArrayOf(0, 0, 0, 0) + "WAVE".toByteArray()
        assertNull(ImageOps.sniff(wav))
    }

    /**
     * ⚠️ HEIC（`ftyp` + `heic`）必须**不**被认成内核支持的格式。
     *
     * 这条是「客户端要转码」这个设计的依据：HEIC 是 iPhone 默认格式，用户从
     * 相册选到的往往就是它，而内核只认三种。这里返回 null 才会走到
     * `resampleToJpeg`，转码而不是直接 400。
     */
    @Test
    fun heic_is_not_mistaken_for_a_supported_format() {
        // ISO BMFF: 4 字节 size + "ftyp" + brand
        val heic = byteArrayOf(0, 0, 0, 0x18) + "ftypheic".toByteArray() + byteArrayOf(0, 0, 0, 0)
        assertNull(ImageOps.sniff(heic))
    }

    /** 太短的输入不能越界（12 字节以下一律 null）。 */
    @Test
    fun short_inputs_are_rejected_without_crashing() {
        assertNull(ImageOps.sniff(ByteArray(0)))
        assertNull(ImageOps.sniff(byteArrayOf(0x89.toByte(), 0x50, 0x4E)))
        assertNull(ImageOps.sniff(ByteArray(11)))
    }
}