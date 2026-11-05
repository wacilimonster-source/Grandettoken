package com.wacil.grandettoken.sync

/** 手机登录的安全边界：只接受零 scope，Client ID 只做基本格式检查。 */
internal object GitHubLoginPolicy {
    private val clientIdPattern = Regex("^[A-Za-z0-9._-]{8,128}$")

    fun validClientId(raw: String): Boolean = clientIdPattern.matches(raw.trim())

    fun isZeroScope(scope: String?): Boolean = scope.isNullOrBlank()
}
