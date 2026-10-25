package dev.agate.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Icon
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.agate.android.ui.theme.agateColors

/**
 * v44 表单输入（.form-input）：inset 底 + 1dp 边框 + 12dp 圆角 + 前缀线性图标 +
 * 透明无框输入。用于「新增提供商」子页的凭据/备注/手机号等（M3-3R 对齐基线）。
 */
@Composable
fun AgateFormInput(
    value: String,
    onValueChange: (String) -> Unit,
    hint: String,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    singleLine: Boolean = true,
    minLines: Int = 1,
    keyboardType: KeyboardType = KeyboardType.Text,
) {
    val C = agateColors()
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(C.insetBg, androidx.compose.foundation.shape.RoundedCornerShape(12.dp))
            .border(1.dp, C.border, androidx.compose.foundation.shape.RoundedCornerShape(12.dp))
            .padding(horizontal = 12.dp, vertical = 10.dp),
        verticalAlignment = if (minLines > 1) Alignment.Top else Alignment.CenterVertically,
    ) {
        icon?.let {
            Icon(
                imageVector = it,
                contentDescription = null,
                tint = C.textDim,
                modifier = Modifier
                    .size(15.dp)
                    .padding(top = if (minLines > 1) 3.dp else 0.dp),
            )
            Box(Modifier.size(8.dp))
        }
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            singleLine = singleLine,
            minLines = minLines,
            textStyle = TextStyle(
                color = C.textMain,
                fontSize = 13.sp,
                fontWeight = FontWeight.Normal,
            ),
            keyboardOptions = KeyboardOptions(keyboardType = keyboardType),
            cursorBrush = androidx.compose.ui.graphics.SolidColor(C.accent),
            modifier = Modifier.fillMaxWidth(),
            decorationBox = { inner ->
                androidx.compose.foundation.layout.Box {
                    inner()
                    if (value.isEmpty()) {
                        androidx.compose.material3.Text(hint, color = C.textDim, fontSize = 13.sp)
                    }
                }
            },
        )
    }
}
