//! 智点明细流水（额度明细看板的数据源）。
//!
//! ── 为什么需要它 ────────────────────────────────────────────
//! 对话接口的响应**不带计费结果**，余额又是延迟归账；只有流水里写着这一次
//! 实际扣了多少（`amount` 为负）、哪一笔加了智点。所以「余额」与「流水」
//! 是两套口径，界面上必须分开显示，不能互相推算。
//!
//! 端点（`ketion/lingxi2api-panel::internal/upstream/lingxi_ledger.go` 从客户端
//! 打包代码反查得到，参数名与原实现一致）：
//! ```text
//!   GET /api/public/v1/credits/transaction/list
//!       ?type=all|income|expense&income_cursor&expense_cursor&limit
//!   GET /api/public/v1/credits/transaction/expense
//!       ?session_id&cursor&limit
//! ```
//!
//! ── 三个必须容错的形状（实抓结论）──────────────────────────
//!   1. `id`：消耗类是数字、获得类（topup）**没有**该字段 —— 声明成 string
//!      会让整页解析失败（面板表现为「整个账号的流水都变空」），所以用
//!      `flex_id` 宽容解析；
//!   2. `type`：获得类是 `topup`（不是 `income`）、消耗是 `expense`，另有
//!      `daily_reset`（日切重置，客户端也会过滤）；
//!   3. 游标是**双向**的：`income` 只认 `income_cursor`、`expense` 只认
//!      `expense_cursor`，两边都给会被上游忽略一半。
//!
//! 硬约束：release 是 `panic=abort`，本文件零 unwrap/expect/panic。

use serde_json::Value;

use super::{client, endpoints};

/// 单页上限（上游口径 50，超过按 50 处理）。
pub const PAGE_LIMIT_MAX: u32 = 50;
/// 单页默认条数。
pub const PAGE_LIMIT_DEFAULT: u32 = 20;

/// 一条流水。
#[derive(Clone, Debug, PartialEq)]
pub struct Txn {
    pub id: String,
    /// `income` / `expense` / `reset`（归一后），未知类型原样保留。
    pub kind: String,
    pub title: String,
    pub source: String,
    /// 带符号智点变化：正 = 获取，负 = 消耗。
    pub amount: f64,
    /// 上游给的时刻串（`time` → `create_time` → `effect_time`），原样透出。
    pub time: String,
    pub expire_time: String,
    pub session_id: String,
}

/// 一页流水 + 双向游标。
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub items: Vec<Txn>,
    pub income_cursor: i64,
    pub expense_cursor: i64,
    pub has_more: bool,
}

fn text_at(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

/// `id` 可能是数字、字符串、缺失或 null —— 一律收敛成字符串（不参与业务判断）。
pub fn flex_id(value: &Value) -> String {
    match value {
        Value::String(text) => text.trim().to_string(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

fn number_at(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|node| {
            node.as_f64()
                .or_else(|| node.as_str().and_then(|text| text.parse::<f64>().ok()))
        })
    })
}

/// 归一成三类。**以金额符号为主、`type` 为辅**（上游两者都可能单边缺失）。
pub fn kind_of(txn_type: &str, amount: f64) -> String {
    if txn_type.eq_ignore_ascii_case("daily_reset") {
        return "reset".to_string();
    }
    if amount > 0.0 {
        return "income".to_string();
    }
    if amount < 0.0 {
        return "expense".to_string();
    }
    match txn_type {
        "topup" | "grant" | "reward" | "income" => "income".to_string(),
        "expense" => "expense".to_string(),
        other if other.is_empty() => String::new(),
        other => other.to_string(),
    }
}

/// 展示名：任务名 → 备注 → 会话号后 8 位 → `—`。
pub fn title_of(task_name: &str, remark: &str, session_id: &str) -> String {
    if !task_name.is_empty() {
        return task_name.to_string();
    }
    if !remark.is_empty() {
        return remark.to_string();
    }
    if !session_id.is_empty() {
        let tail: String = session_id
            .chars()
            .rev()
            .take(8)
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        return format!("会话 {tail}");
    }
    "—".to_string()
}

/// 来源展示：上游 `source` 是中文（如「灵犀客户端」），获得类通常没有；
/// `system` 不算来源，用备注/`sku_key` 兜底。
pub fn source_of(source: &str, remark: &str, sku_key: &str) -> String {
    if !source.is_empty() && !source.eq_ignore_ascii_case("system") {
        return source.to_string();
    }
    if !remark.is_empty() {
        return remark.to_string();
    }
    sku_key.to_string()
}

/// 解析一条流水（纯函数）。
pub fn parse_txn(item: &Value) -> Option<Txn> {
    let txn_type = text_at(item, &["type"]);
    let amount = number_at(item, &["amount"]).unwrap_or(0.0);
    let task_name = text_at(item, &["task_name"]);
    let remark = text_at(item, &["remark"]);
    let sku_key = text_at(item, &["sku_key"]);
    let session_id = text_at(item, &["session_id"]);
    // 一条既没有金额也没有类型的记录没有信息量（可能是上游的占位行）。
    if amount == 0.0 && txn_type.is_empty() {
        return None;
    }
    Some(Txn {
        id: flex_id(item.get("id").unwrap_or(&Value::Null)),
        kind: kind_of(&txn_type, amount),
        title: title_of(&task_name, &remark, &session_id),
        source: source_of(&text_at(item, &["source"]), &remark, &sku_key),
        amount,
        time: {
            let value = text_at(item, &["time", "create_time", "effect_time"]);
            value
        },
        expire_time: text_at(item, &["expire_time"]),
        session_id,
    })
}

/// 解析一页（纯函数）。`data.list[]` 是主口径，`data.income_cursor` /
/// `expense_cursor` / `has_more` 是分页游标。
pub fn parse_page(body: &Value) -> Page {
    let data = body.get("data").unwrap_or(body);
    let items = data
        .get("list")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(parse_txn).collect::<Vec<Txn>>())
        .unwrap_or_default();
    Page {
        items,
        income_cursor: data.get("income_cursor").and_then(Value::as_i64).unwrap_or(0),
        expense_cursor: data.get("expense_cursor").and_then(Value::as_i64).unwrap_or(0),
        has_more: data.get("has_more").and_then(Value::as_bool).unwrap_or(false),
    }
}

/// 查询串。`type` 决定给哪一边的游标（双向游标是上游的硬要求）。
pub fn list_query(typ: &str, income_cursor: i64, expense_cursor: i64, limit: u32) -> String {
    let typ = if typ.is_empty() { "all" } else { typ };
    let limit = if limit == 0 { PAGE_LIMIT_DEFAULT } else { limit.min(PAGE_LIMIT_MAX) };
    let mut query = format!("type={typ}");
    if typ != "expense" {
        query.push_str(&format!("&income_cursor={income_cursor}"));
    }
    if typ != "income" {
        query.push_str(&format!("&expense_cursor={expense_cursor}"));
    }
    // `limit` 与游标无关，三种 type 都要带 —— 只给一边会把另一个方向截成默认长度。
    query.push_str(&format!("&limit={limit}"));
    query
}

/// 取一页流水。上游非 2xx 直接交调用方（界面写「暂不可用」并带上状态码）。
pub async fn page(
    sid: &str,
    device_id: &str,
    cookie: Option<&str>,
    typ: &str,
    income_cursor: i64,
    expense_cursor: i64,
    limit: u32,
) -> Result<Page, u32> {
    let path = format!(
        "{}?{}",
        endpoints::transaction_list_path(),
        list_query(typ, income_cursor, expense_cursor, limit)
    );
    let reply = match client::request("GET", &path, sid, device_id, cookie, None).await {
        Ok(reply) => reply,
        Err(_) => return Err(502),
    };
    if !(200..300).contains(&reply.status) || !reply.result_ok {
        return Err(u32::try_from(reply.status).unwrap_or(0));
    }
    Ok(parse_page(&reply.body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page_body() -> Value {
        json!({ "result": "ok", "data": {
            "list": [
                { "id": 28376339, "type": "expense", "amount": -1.5,
                  "task_name": "解释 TCP 三次握手", "source": "灵犀客户端",
                  "time": "2026-11-14 10:02:11", "session_id": "sess-abcdefgh1234" },
                { "type": "topup", "amount": 3, "remark": "每日签到",
                  "create_time": "2026-11-14 09:00:00", "expire_time": "2026-12-14 09:00" },
                { "type": "daily_reset", "amount": 0, "remark": "日切重置" },
                { "amount": 0 }
            ],
            "income_cursor": 111, "expense_cursor": 222, "has_more": true
        } })
    }

    #[test]
    fn numeric_and_missing_ids_both_parse() {
        let page = parse_page(&page_body());
        assert_eq!(page.items.len(), 3, "占位行（无金额无类型）要丢掉");
        assert_eq!(page.items[0].id, "28376339");
        assert_eq!(page.items[1].id, "", "获得类没有 id 字段是正常形状");
    }

    #[test]
    fn kind_follows_the_amount_sign_then_the_type() {
        assert_eq!(kind_of("expense", -1.5), "expense");
        assert_eq!(kind_of("topup", 3.0), "income");
        assert_eq!(kind_of("daily_reset", 0.0), "reset");
        assert_eq!(kind_of("topup", 0.0), "income");
        assert_eq!(kind_of("", 0.0), "");
    }

    #[test]
    fn title_and_source_fall_back_in_the_documented_order() {
        assert_eq!(title_of("任务", "备注", "s"), "任务");
        assert_eq!(title_of("", "备注", "s"), "备注");
        assert_eq!(title_of("", "", "sess-abcdefgh1234"), "会话 efgh1234");
        assert_eq!(title_of("", "", ""), "—");
        assert_eq!(source_of("system", "备注", "sku"), "备注");
        assert_eq!(source_of("", "", "sku"), "sku");
    }

    #[test]
    fn page_carries_both_cursors_and_has_more() {
        let page = parse_page(&page_body());
        assert_eq!(page.income_cursor, 111);
        assert_eq!(page.expense_cursor, 222);
        assert!(page.has_more);
        assert_eq!(page.items[0].expire_time, "");
        assert_eq!(page.items[1].expire_time, "2026-12-14 09:00");
    }

    #[test]
    fn cursor_is_only_sent_to_the_side_the_type_asks_for() {
        let all = list_query("all", 1, 2, 20);
        assert!(all.contains("income_cursor=1") && all.contains("expense_cursor=2"));
        let income = list_query("income", 1, 2, 20);
        assert!(income.contains("income_cursor=1"));
        assert!(!income.contains("expense_cursor"), "{income}");
        let expense = list_query("expense", 1, 2, 20);
        assert!(expense.contains("expense_cursor=2"));
        assert!(!expense.contains("income_cursor"), "{expense}");
        // limit 三种 type 都要带（漏了会被上游按默认长度截断）
        for query in [&all, &income, &expense] {
            assert!(query.contains("limit=20"), "{query}");
        }
    }

    #[test]
    fn limit_is_clamped_to_the_upstream_maximum() {
        assert!(list_query("all", 0, 0, 999).contains("limit=50"));
        assert!(list_query("all", 0, 0, 0).contains("limit=20"));
    }

    #[test]
    fn malformed_payload_yields_an_empty_page_not_a_panic() {
        let page = parse_page(&json!({}));
        assert!(page.items.is_empty());
        assert_eq!(page.income_cursor, 0);
        assert!(!page.has_more);
    }
}
