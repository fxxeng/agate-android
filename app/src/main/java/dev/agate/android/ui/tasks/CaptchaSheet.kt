package dev.agate.android.ui.tasks

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import dev.agate.android.data.CheckinOps
import dev.agate.android.ui.components.AgateFormInput
import dev.agate.android.ui.theme.AgateType
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch
import org.json.JSONObject

/**
 * 验证码池补货节奏（`GET/PUT /api/zcode/captcha-pool`）。
 *
 * ── 只有三个数，没有「过码」按钮 ─────────────────────────────
 * `target`（库存目标）/ `pollMs`（轮询间隔）/ `maxPerRound`（单轮上限）。
 * 上游本身没有「手动过码」的界面：令牌由 WebView 里的后台守卫铸，
 * 守卫脚本的文件头第一句就在解释「为什么不做成用户可见的按钮」。
 * Android 端要做需要内嵌 WebView + 阿里云滑块 SDK —— 与本轮范围差得远，
 * 所以这里**留空位并说明原因**，而不是画一个点了没反应的按钮。
 *
 * ── 保存返回生效后的全量值 ──────────────────────────────────
 * PUT 的响应是生效后的三项（不是回显请求体）。所以保存后直接用响应刷新，
 * 不必再 GET 一次 —— 与 `/api/queue` 同一模式。
 * 三项里有**一项非法时整体不落盘**：避免「目标改了、节奏没改」的半套设置。
 */
@Composable
fun CaptchaSheet(
    onDismiss: () -> Unit,
    onSaved: () -> Unit,
) {
    val C = agateColors()
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var target by remember { mutableStateOf("") }
    var pollMs by remember { mutableStateOf("") }
    var maxPerRound by remember { mutableStateOf("") }
    var loading by remember { mutableStateOf(true) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    fun toast(text: String) = Toast.makeText(ctx, text, Toast.LENGTH_SHORT).show()

    fun fill(obj: JSONObject) {
        target = obj.optInt("zcodeCaptchaPoolTarget", 0).toString()
        pollMs = obj.optInt("zcodeCaptchaPoolPollMs", 0).toString()
        maxPerRound = obj.optInt("zcodeCaptchaPoolMaxPerRound", 0).toString()
    }

    LaunchedEffect(Unit) {
        val data = CheckinOps.captchaPool()
        loading = false
        if (data != null) fill(data) else error = "读不到当前节奏设置"
    }

    Dialog(onDismissRequest = onDismiss) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(C.cardBg, RoundedCornerShape(20.dp))
                .border(1.dp, C.border, RoundedCornerShape(20.dp))
                .padding(18.dp),
        ) {
            Text("验证码池节奏", color = C.textMain, fontSize = 16.sp, fontWeight = FontWeight.Bold)
            Spacer(Modifier.height(4.dp))
            Text(
                "库存目标 · 轮询间隔 · 单轮铸造上限",
                style = AgateType.Caption,
                color = C.textDim,
            )
            Spacer(Modifier.height(12.dp))

            if (loading) {
                Text("读取中…", style = AgateType.Caption, color = C.textDim)
            } else if (error != null) {
                Text(error!!, style = AgateType.BodyS, color = C.red)
            } else {
                NumberField("库存目标", target, "低于它才补货；出厂 3") { target = it }
                Spacer(Modifier.height(8.dp))
                NumberField("轮询间隔（毫秒）", pollMs, "出厂 2500") { pollMs = it }
                Spacer(Modifier.height(8.dp))
                NumberField("单轮铸造上限", maxPerRound, "出厂 1") { maxPerRound = it }
            }

            Spacer(Modifier.height(10.dp))
            TaskFold(
                "调大烧的是阿里云风控配额，过期令牌一律作废。撞上限之后是整段铸不出来，比多等一会儿严重。" +
                    "「手动过码」本版不做 —— 上游也没有这个界面，令牌由后台守卫铸。"
            )

            Spacer(Modifier.height(12.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Spacer(Modifier.weight(1f))
                TaskBtn("取消") { onDismiss() }
                TaskBtn("保存", primary = true, enabled = !busy && !loading) {
                    val t = target.trim().toIntOrNull()
                    val p = pollMs.trim().toIntOrNull()
                    val m = maxPerRound.trim().toIntOrNull()
                    if (t == null || p == null || m == null) {
                        toast("三项都必须是整数")
                        return@TaskBtn
                    }
                    busy = true
                    scope.launch {
                        val r = CheckinOps.saveCaptchaPool(t, p, m)
                        busy = false
                        if (r.ok) {
                            // 响应是**生效后**的全量值，直接用它回填
                            r.data?.let { fill(it) }
                            toast("节奏已保存，下一轮轮询就按新值跑")
                            onSaved()
                        } else {
                            toast(r.error ?: "保存失败")
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun NumberField(
    label: String,
    value: String,
    hint: String,
    onValueChange: (String) -> Unit,
) {
    val C = agateColors()
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(label, color = C.textSub, fontSize = 12.sp)
            Text(hint, style = AgateType.Caption, color = C.textDim)
        }
        AgateFormInput(
            value = value,
            onValueChange = { text -> onValueChange(text.filter { it.isDigit() }) },
            hint = "0",
            keyboardType = KeyboardType.Number,
            modifier = Modifier.padding(start = 8.dp),
        )
    }
}
