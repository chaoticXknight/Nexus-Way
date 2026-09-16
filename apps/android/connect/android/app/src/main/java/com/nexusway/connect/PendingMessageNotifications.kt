package com.nexusway.connect

internal fun pendingMessageNotifications(
    pending: List<DirectMessage>,
    incoming: List<DirectMessage>,
    delivered: Set<String>,
    deleted: Set<String>,
): List<DirectMessage> = (pending + incoming)
    .filterNot { it.mine || it.id in delivered || it.id in deleted }
    .associateBy { it.id }.values.sortedBy { it.sent }