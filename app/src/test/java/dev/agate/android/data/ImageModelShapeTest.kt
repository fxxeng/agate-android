package dev.agate.android.data

import org.json.JSONObject
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * 生图模型的判别（`JSONObject.isImageModel()`）。
 *
 * ── 为什么这条值得单独测 ──────────────────────────────────
 * 判据本身只有两行，但它决定界面怎么画一条模型：判错的方向是**双向**的
 *   - 漏判（把生图模型当对话模型）：模型页给它画「工具✗ 图片✗ 思考✗」三连灰，
 *     用户完全看不出它是什么 —— 生图通道接通之前清单里没有这类条目，
 *     所以这个错**只在接通之后才可能出现**；
 *   - 误判（把对话模型当生图模型）：模型页给它打「生图」标签，用户拿它去生图，
 *     而它根本产不出图片。
 * 两个方向都没有编译期信号，只能靠用例钉住。
 */
class ImageModelShapeTest {

    /** 内核 `shape::list_item` 的形态（`/api/session` 走这条）。 */
    @Test
    fun snake_case_output_modalities_counts() {
        val item = JSONObject(
            """{"id":"gemini-3-pro-image","kind":"image","output_modalities":["image"]}""",
        )
        assertTrue(item.isImageModel())
    }

    /** 内核 `manage_view` 的形态（`/api/models/manage` 走这条，键名是驼峰）。 */
    @Test
    fun camel_case_output_modalities_counts() {
        val item = JSONObject(
            """{"id":"raccoon-image","kind":"image","outputModalities":["image"]}""",
        )
        assertTrue(item.isImageModel())
    }

    /** 只给了 `output_modalities`、没有 `kind`：也算（两个判据互为补充）。 */
    @Test
    fun modalities_alone_are_enough() {
        assertTrue(JSONObject("""{"id":"x","output_modalities":["text","image"]}""").isImageModel())
    }

    /** `type` 是老形态的写法，与 `kind` 同义。 */
    @Test
    fun type_is_an_alias_for_kind() {
        assertTrue(JSONObject("""{"id":"x","type":"IMAGE"}""").isImageModel())
    }

    /**
     * ⚠️ 名字里带 `image` 的**对话**模型不能算生图模型。
     *
     * 这是最容易犯的误判：多模态输入模型（能看图）与生图模型（产出图）在中文里
     * 都叫「图片模型」，而它们的 `supports_images` 恰恰相反 —— 一个为 true
     * （收图），一个为 false（不收图，只出图）。
     */
    @Test
    fun chat_models_are_not_image_models_even_when_named_like_one() {
        val vision = JSONObject("""{"id":"qwen-image-understanding","supports_images":true}""")
        assertFalse(vision.isImageModel())
        val plain = JSONObject("""{"id":"glm-4.6v","kind":"chat"}""")
        assertFalse(plain.isImageModel())
    }

    /** 三个键都缺：不能因为「读不到」就当它是生图模型。 */
    @Test
    fun missing_fields_fall_back_to_not_an_image_model() {
        assertFalse(JSONObject("""{"id":"some-chat"}""").isImageModel())
        assertFalse(JSONObject("""{"id":"some-chat","kind":""}""").isImageModel())
        // `output_modalities` 是空数组也不算
        assertFalse(JSONObject("""{"id":"some-chat","output_modalities":[]}""").isImageModel())
    }
}