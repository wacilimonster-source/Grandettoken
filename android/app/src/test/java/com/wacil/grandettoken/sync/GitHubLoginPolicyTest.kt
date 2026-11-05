package com.wacil.grandettoken.sync

import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class GitHubLoginPolicyTest {
    @Test
    fun accepts_public_client_id_shapes() {
        assertTrue(GitHubLoginPolicy.validClientId("Iv1.0123456789abcdef"))
        assertTrue(GitHubLoginPolicy.validClientId(" Ov23liAbCdEf123456 "))
    }

    @Test
    fun rejects_empty_or_malformed_client_ids() {
        assertFalse(GitHubLoginPolicy.validClientId(""))
        assertFalse(GitHubLoginPolicy.validClientId("has space here"))
        assertFalse(GitHubLoginPolicy.validClientId("短"))
    }

    @Test
    fun only_zero_scope_is_accepted() {
        assertTrue(GitHubLoginPolicy.isZeroScope(null))
        assertTrue(GitHubLoginPolicy.isZeroScope(""))
        assertFalse(GitHubLoginPolicy.isZeroScope("gist"))
        assertFalse(GitHubLoginPolicy.isZeroScope("read:user"))
    }
}
