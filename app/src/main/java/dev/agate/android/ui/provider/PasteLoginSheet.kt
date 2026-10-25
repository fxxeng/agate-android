package dev.agate.android.ui.provider

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.data.AccountOps
import dev.agate.android.data.ProviderPreset
import dev.agate.android.ui.theme.agateColors
import kotlinx.coroutines.launch

/**
 * 粘贴凭证添加底弹（M3-4）：kuku（Cookie 整串）/ commandcode（user_ API Key）/
 * antigravity（Google refresh token，email 可选）。
 *
 * 提交即调 POST /api/accounts（统一入口），上游会校验凭证真实性（kuku 补昵称、
 * commandcode 查前缀形状），失败原因原样透出。上游校验走网络，20s 读超时。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PasteLoginSheet(
    preset: ProviderPreset,
    onDismiss: () -> Unit,
    onResult: (success: Boolean, message: String) -> Unit,
) {
    val C = agateColors()
    val scope = rememberCoroutineScope()
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)

    var primary by remember { mutableStateOf("") }
    var secondary by remember { mutableStateOf("") }
    var name by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var hint by remember { mutableStateOf<String?>(null) }

    // 各家主凭证的表单形态
    val primaryLabel: String
    val primaryHint: String
    val minLines: Int
    var showSecondary = false
    var secondaryLabel = ""
    when (preset.id) {
        "kuku" -> {
            primaryLabel = "Cookie 整串"
            primaryHint = "登录 wenku.baidu.com 后复制完整 Cookie（含 BDUSS 与 STOKEN）"
            minLines = 4
        }
        "commandcode" -> {
            primaryLabel = "API Key"
            primaryHint = "以 user_ 开头的密钥"
            minLines = 1
        }
        "antigravity" -> {
            primaryLabel = "Google refresh token"
            primaryHint = "Google OAuth 的 refresh_token（1// 开头）"
            minLines = 3
            showSecondary = true
            secondaryLabel = "Google 账号邮箱（可选，用于同账号去重）"
        }
        else -> {
            primaryLabel = "凭证"
            primaryHint = ""
            minLines = 3
        }
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = C.cardBg,
    ) {
        Column(Modifier.padding(horizontal = 16.dp)) {
            Text("添加 ${preset.label}", color = C.textMain, fontSize = 18.sp, fontWeight = FontWeight.Bold)
            Text("粘贴凭证 · 提交时向 ${preset.label} 上游校验", color = C.textDim, fontSize = 12.sp)
            Spacer(Modifier.height(14.dp))

            OutlinedTextField(
                value = primary,
                onValueChange = { primary = it },
                label = { Text(primaryLabel) },
                supportingText = { Text(primaryHint, color = C.textDim) },
                minLines = minLines,
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
            )
            if (showSecondary) {
                Spacer(Modifier.height(10.dp))
                OutlinedTextField(
                    value = secondary,
                    onValueChange = { secondary = it.trim() },
                    label = { Text(secondaryLabel) },
                    singleLine = true,
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth(),
                )
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
                Text(it, color = C.red, fontSize = 12.sp)
                Spacer(Modifier.height(6.dp))
            }

            Button(
                onClick = {
                    if (primary.isBlank() || busy) return@Button
                    busy = true
                    hint = null
                    scope.launch {
                        val r = AccountOps.addAccountFor(
                            preset.id, primary,
                            secondary.ifBlank { null },
                            name.ifBlank { null },
                        )
                        busy = false
                        if (r.ok) onResult(true, "已添加 ${preset.label} 账号")
                        else hint = r.error ?: "添加失败"
                    }
                },
                enabled = !busy && primary.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) { Text(if (busy) "校验中…" else "添加") }
            Spacer(Modifier.height(18.dp))
        }
    }
}
