package dev.agate.android.ui.provider

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AccountOps
import dev.agate.android.data.ProviderPreset
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * 手机验证码登录底弹（M3-4）：autoclaw / autoclaw-intl / loomy。
 *
 * 两段式：发码（60s 倒计时防重发）→ 输码提交。loomy 的 msgid 从 send 响应取、
 * verify 时回传（内核 login_with_code 靠它对号）。成功后由调用方刷新快照。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SmsLoginSheet(
    preset: ProviderPreset,
    onDismiss: () -> Unit,
    onResult: (success: Boolean, message: String) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)

    var phone by remember { mutableStateOf("") }
    var code by remember { mutableStateOf("") }
    var name by remember { mutableStateOf("") }
    var msgid by remember { mutableStateOf<String?>(null) }
    var sent by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var countdown by remember { mutableStateOf(0) }
    var hint by remember { mutableStateOf<String?>(null) }

    // 发码后 60s 倒计时（防重发轰炸上游）
    LaunchedEffect(sent) {
        if (sent && countdown == 0) countdown = 60
        while (isActive && countdown > 0) {
            delay(1_000L)
            countdown -= 1
        }
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = C.cardBg,
    ) {
        Column(Modifier.padding(horizontal = 16.dp)) {
            Text("登录 ${preset.label}", color = C.textMain, fontSize = 18.sp, fontWeight = FontWeight.Bold)
            Text("手机号接收验证码 · 登录后直接落成账号", color = C.textDim, fontSize = 12.sp)
            Spacer(Modifier.height(14.dp))

            OutlinedTextField(
                value = phone,
                onValueChange = { phone = it.trim() },
                label = { Text("手机号") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Phone),
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(10.dp))

            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = code,
                    onValueChange = { code = it.trim() },
                    label = { Text("验证码") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    enabled = !busy && sent,
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.width(10.dp))
                OutlinedButton(
                    onClick = {
                        if (phone.length < 5 || busy) return@OutlinedButton
                        busy = true
                        hint = null
                        scope.launch {
                            val r = AccountOps.smsSend(preset.id, phone)
                            busy = false
                            if (r.ok) {
                                sent = true
                                msgid = r.data?.optString("msgid")?.ifBlank { null }
                                hint = "验证码已发送，请查收短信"
                            } else {
                                hint = r.error ?: "发送失败"
                            }
                        }
                    },
                    enabled = !busy && countdown == 0 && phone.length >= 5,
                ) {
                    Text(if (countdown > 0) "${countdown}s" else if (sent) "重发" else "发送验证码")
                }
            }
            Spacer(Modifier.height(10.dp))

            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text("备注名（可选）") },
                singleLine = true,
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(6.dp))

            hint?.let {
                Text(it, color = if (it.contains("已发送")) C.green else C.red, fontSize = 12.sp)
                Spacer(Modifier.height(6.dp))
            }

            Button(
                onClick = {
                    if (code.length < 4 || busy) return@Button
                    busy = true
                    hint = null
                    scope.launch {
                        val r = AccountOps.smsVerify(preset.id, phone, code, msgid, name.ifBlank { null })
                        busy = false
                        if (r.ok) onResult(true, "已添加 ${preset.label} 账号")
                        else hint = r.error ?: "登录失败"
                    }
                },
                enabled = !busy && sent && phone.length >= 5 && code.length >= 4,
                modifier = Modifier.fillMaxWidth(),
            ) { Text(if (busy) "处理中…" else "登录") }
            Spacer(Modifier.height(18.dp))
        }
    }
}
