package dev.agate.core

/** 旧 action 与 data URI 的路由判据；外部输入在此归类后由单一 Receiver 处理。 */
internal object AgateAlarmRouter {
    fun resource(action: String?, data: String?): AlarmResource? {
        if (action == AlarmScheduler.ACTION && data == "agalarm://tick/main") {
            return AlarmResource.Checkin
        }
        if (action == AgateAlarmIntents.ACTION_REFRESH && data != null) {
            return when (data) {
                "agalarm://refresh/gateway" -> AlarmResource.GatewayRefresh
                "agalarm://refresh/usage" -> AlarmResource.UsageRefresh
                "agalarm://refresh/credentials" -> AlarmResource.CredentialRefresh
                else -> null
            }
        }
        return null
    }
}

enum class AlarmResource(val key: String) {
    Checkin("checkin"),
    GatewayRefresh("refresh:gateway"),
    UsageRefresh("refresh:usage"),
    CredentialRefresh("refresh:credentials"),
}
