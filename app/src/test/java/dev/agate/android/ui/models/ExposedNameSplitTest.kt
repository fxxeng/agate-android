package dev.agate.android.ui.models

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * 拆对外名（`splitExposedName` / `wireModelId`）。
 *
 * ── 为什么要钉这几条（2026-11-16 的真实故障）─────────────────
 * 页面上曾统一写 `if ('/' in id) id.substringAfter('/') else id` —— 把第一段
 * **无条件**当成网关的 `provider/` 前缀。魔搭等家的上游 id 本身是 `组织/模型名`
 * （`deepseek-ai/DeepSeek-V4-Flash-0731`），裸形态被这么一切就只剩模型段，于是
 * 它在管理视图里查不到自己：开关点了只弹「这个模型不在当前目录清单里，启停对它
 * 无效」，可用账号数也按错的 id 去问。
 *
 * 判据与内核 `split_provider_prefix` 同一条：**只有第一段确实是这家网关的
 * provider id 时**才是前缀。
 */
class ExposedNameSplitTest {

    /** 裸形态：带上 provider 也不该切组织段（这就是那次故障的原始形状） */
    @Test
    fun bare_modelscope_id_keeps_its_org_segment() {
        val id = "deepseek-ai/DeepSeek-V4-Flash-0731"
        assertEquals(id, wireModelId(id, "modelscope"))
        assertEquals(null to id, splitExposedName(id, "modelscope"))
    }

    /** provider 形态：前缀确实是这家 ⇒ 剥掉第一段，剩下的组织段原样保留 */
    @Test
    fun provider_prefixed_id_is_unwrapped_once() {
        val id = "modelscope/deepseek-ai/DeepSeek-V4-Flash-0731"
        assertEquals("deepseek-ai/DeepSeek-V4-Flash-0731", wireModelId(id, "modelscope"))
        assertEquals(
            "modelscope" to "deepseek-ai/DeepSeek-V4-Flash-0731",
            splitExposedName(id, "modelscope"),
        )
    }

    /** 前缀判定大小写不敏感（与内核 `eq_ignore_ascii_case` 同一口径） */
    @Test
    fun prefix_match_ignores_case() {
        assertEquals("DeepSeek-V4-Pro", wireModelId("ModelScope/DeepSeek-V4-Pro", "modelscope"))
    }

    /** 不知道是哪家时（`list-view` 的 ids）：第一段在注册表里才算前缀 */
    @Test
    fun without_provider_the_registry_decides() {
        assertEquals("modelscope" to "Qwen/Qwen3.8-Flash-Next", splitExposedName("modelscope/Qwen/Qwen3.8-Flash-Next"))
        // `deepseek-ai` 不是注册表里的 provider id ⇒ 整串是上游 id，不切
        assertEquals(null to "deepseek-ai/DeepSeek-V4-Flash-0731", splitExposedName("deepseek-ai/DeepSeek-V4-Flash-0731"))
    }

    /** 别名行（对外名里没有斜杠）必须原样返回 */
    @Test
    fun alias_without_slash_is_untouched() {
        assertEquals(null to "glm-5.3", splitExposedName("glm-5.3", "raccoon"))
        assertEquals("glm-5.3", wireModelId("glm-5.3", "raccoon"))
    }

    /** 空串 / 尾斜杠这类脏值不 panic，也不产生空模型名 */
    @Test
    fun degenerate_inputs_are_returned_as_is() {
        assertEquals(null to "", splitExposedName("", "modelscope"))
        assertEquals(null to "modelscope/", splitExposedName("modelscope/", "modelscope"))
        assertNull(splitExposedName("/x").first)
    }
}
