// Owns reusable Connect UI pieces such as avatars, post cards, reactions, and
// comments. It does not load or mutate HIVE data; ConnectViewModel owns that.

package com.nexusway.connect

import android.content.Intent
import android.widget.MediaController
import android.widget.VideoView
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Hexagon
import androidx.compose.material.icons.outlined.Hexagon
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withLink
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import coil.compose.AsyncImage
import java.text.DateFormat
import java.util.Date

val REACTIONS = listOf("❤️", "👍", "😂", "😮", "😢", "😠")

fun timeAgo(unixSeconds: Long): String {
    val delta = System.currentTimeMillis() / 1000 - unixSeconds
    return when {
        delta < 60 -> "now"
        delta < 3600 -> "${delta / 60}m"
        delta < 86_400 -> "${delta / 3600}h"
        delta < 7 * 86_400 -> "${delta / 86_400}d"
        else -> DateFormat.getDateInstance(DateFormat.SHORT).format(Date(unixSeconds * 1000))
    }
}

fun audienceTag(audience: String, foldName: String? = null): String = when {
    audience == "public" -> "🌐 public"
    audience == "followers" -> "👥 followers"
    audience.startsWith("circle:") -> "🔒 " + (foldName ?: "fold")
    else -> audience
}

/** The report-reason set — keeps the moderation queue reviewable. */
val REPORT_REASONS = listOf(
    "Harassment or bullying",
    "Spam",
    "Violence or threats",
    "Sexual content",
    "Copyright violation",
    "Other",
)

internal data class MentionMatch(
    val start: Int,
    val endExclusive: Int,
    val handle: String,
)

internal fun findMentionMatches(body: String, knownHandles: Collection<String>): List<MentionMatch> {
    val handles = knownHandles.filter(String::isNotBlank).distinct().sortedByDescending(String::length)
    val matches = mutableListOf<MentionMatch>()
    var at = body.indexOf('@')
    while (at >= 0) {
        if (at == 0 || !body[at - 1].isLetterOrDigit()) {
            val start = at + 1
            val quotedEnd = if (body.getOrNull(start) == '"') body.indexOf('"', start + 1) else -1
            val quotedHandle = if (quotedEnd > start + 1) body.substring(start + 1, quotedEnd) else null
            val knownHandle = handles.firstOrNull { handle ->
                body.regionMatches(start, handle, 0, handle.length) &&
                    body.getOrNull(start + handle.length)?.let { next ->
                        !next.isLetterOrDigit() && next !in "_.-"
                    } != false
            }
            val handle = quotedHandle ?: knownHandle ?: run {
                body.substring(start)
                    .takeWhile { it.isLetterOrDigit() || it in "_.-" }
                    .trimEnd('.', '-')
                    .takeIf(String::isNotEmpty)
            }
            if (handle != null) {
                val end = if (quotedHandle != null) quotedEnd + 1 else start + handle.length
                matches += MentionMatch(at, end, handle)
                at = body.indexOf('@', end)
                continue
            }
        }
        at = body.indexOf('@', at + 1)
    }
    return matches
}

/** Body text with tappable @handle mentions — tap opens the profile. */
@Composable
fun MentionText(
    vm: ConnectViewModel,
    body: String,
    fontSize: androidx.compose.ui.unit.TextUnit,
    lineHeight: androidx.compose.ui.unit.TextUnit = androidx.compose.ui.unit.TextUnit.Unspecified,
    modifier: Modifier = Modifier,
) {
    val mentionColor = Accent2
    val knownHandles = vm.knownMentionHandles
    val annotated = remember(body, mentionColor, knownHandles) {
        androidx.compose.ui.text.buildAnnotatedString {
            var last = 0
            for (mention in findMentionMatches(body, knownHandles)) {
                append(body.substring(last, mention.start))
                withLink(
                    androidx.compose.ui.text.LinkAnnotation.Clickable(
                        tag = mention.handle,
                        styles = androidx.compose.ui.text.TextLinkStyles(
                            style = androidx.compose.ui.text.SpanStyle(
                                color = mentionColor, fontWeight = FontWeight.SemiBold,
                            ),
                        ),
                    ) { vm.openProfile(mention.handle) },
                ) { append(body.substring(mention.start, mention.endExclusive)) }
                last = mention.endExclusive
            }
            append(body.substring(last))
        }
    }
    Text(annotated, color = TextMain, fontSize = fontSize, lineHeight = lineHeight, modifier = modifier)
}

/** Avatar image or a colored initial disc. */
@Composable
fun Avatar(avatarBlob: String?, name: String, size: Dp, vm: ConnectViewModel? = null) {
    val api = vm?.api
    if (avatarBlob != null && api != null) {
        AsyncImage(
            model = api.mediaUrl(avatarBlob),
            contentDescription = null,
            contentScale = ContentScale.Crop,
            modifier = Modifier.size(size).clip(CircleShape),
        )
    } else if (avatarBlob != null) {
        // Bottom-bar avatar renders before vm is threaded; Coil's local
        // ImageLoader still carries auth via CompositionLocal.
        AsyncImage(
            model = avatarBlob.takeIf { it.startsWith("http") },
            contentDescription = null,
            contentScale = ContentScale.Crop,
            modifier = Modifier.size(size).clip(CircleShape).background(PanelHi),
        )
    } else {
        Box(
            Modifier.size(size).clip(CircleShape).background(BrandBrush),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                name.trim().take(1).uppercase().ifEmpty { "?" },
                color = TextMain,
                fontSize = (size.value * 0.45f).sp,
                fontWeight = FontWeight.Bold,
            )
        }
    }
}

/** Author avatar that resolves through the ViewModel (has the media URL). */
@Composable
fun AuthorAvatar(vm: ConnectViewModel, author: Author, size: Dp) {
    val api = vm.api
    if (author.avatarBlob != null && api != null) {
        AsyncImage(
            model = api.mediaUrl(author.avatarBlob),
            contentDescription = null,
            contentScale = ContentScale.Crop,
            modifier = Modifier.size(size).clip(CircleShape).background(PanelHi),
        )
    } else {
        Avatar(null, author.label.removePrefix("@"), size)
    }
}

/** Account identity marker: Admin, Steward, paid member, or founding beta member. */
@Composable
fun RoleHexBadge(
    founder: Boolean,
    communityRole: String,
    membershipTier: String,
    modifier: Modifier = Modifier,
) {
    val outlined = !founder && (communityRole == "steward" || membershipTier == "paid")
    val gold = founder || communityRole == "steward"
    val description = when {
        founder -> "Nexus-Way Admin"
        communityRole == "steward" -> "Community Steward"
        membershipTier == "paid" -> "Paid member"
        else -> "Founding beta member"
    }
    Icon(
        imageVector = if (outlined) Icons.Outlined.Hexagon else Icons.Filled.Hexagon,
        contentDescription = description,
        tint = if (gold) Honey else Accent2,
        modifier = modifier.size(16.dp),
    )
}

/** One row of an account: avatar, name, handle, trailing action. */
@Composable
fun AuthorRow(
    vm: ConnectViewModel,
    author: Author,
    onOpen: () -> Unit,
    trailing: @Composable () -> Unit = {},
) {
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onOpen)
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        AuthorAvatar(vm, author, 44.dp)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(author.label, color = TextMain, fontWeight = FontWeight.SemiBold)
            if (author.displayName.isNotEmpty()) {
                Text("@${author.handle}", color = TextDim, fontSize = 13.sp)
            }
        }
        trailing()
    }
}

// ---------------------------------------------------------------- posts

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun PostCard(vm: ConnectViewModel, p: Post, showAuthor: Boolean = true) {
    val context = LocalContext.current
    val folded = p.audience.startsWith("circle:")
    val foldOwner = folded && vm.folds.any {
        it.circleId == p.audience.removePrefix("circle:") && it.owned
    }
    var reactionsOpen by remember { mutableStateOf(false) }
    var menuOpen by remember { mutableStateOf(false) }
    var editOpen by remember { mutableStateOf(false) }
    var editDraft by remember { mutableStateOf("") }
    var reportOpen by remember { mutableStateOf(false) }
    var warningRevealed by remember(p.postId, p.contentWarning) {
        mutableStateOf(p.contentWarning.isBlank())
    }

    Surface(
        color = Panel,
        shape = HiveCutShape,
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 5.dp)
            .hivePanelDepth(),
    ) {
        Column(Modifier.padding(bottom = 6.dp)) {
            if (showAuthor) {
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clickable { vm.openProfile(p.author.accountId) }
                        .padding(start = 12.dp, end = 4.dp, top = 10.dp, bottom = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    AuthorAvatar(vm, p.author, 40.dp)
                    Spacer(Modifier.width(10.dp))
                    Column(Modifier.weight(1f)) {
                        Text(p.author.label, color = TextMain, fontWeight = FontWeight.SemiBold)
                        Text(
                            (if (p.pinned) "Pinned · " else "") +
                                "${timeAgo(p.created)} · ${audienceTag(p.audience, vm.foldName(p.audience))}" +
                                if (p.edited != null) " · edited" else "",
                            color = TextMuted, fontSize = 12.sp,
                        )
                    }
                    Box {
                        IconButton(onClick = { menuOpen = true }) {
                            Text("⋯", color = TextDim, fontSize = 18.sp)
                        }
                        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                            if (p.author.accountId == vm.accountId) {
                                if (!folded) {
                                    DropdownMenuItem(
                                        text = { Text(if (p.pinned) "Unpin post" else "Pin to profile") },
                                        onClick = { menuOpen = false; vm.togglePinPost(p) },
                                    )
                                    DropdownMenuItem(
                                        text = { Text("Edit post") },
                                        onClick = { menuOpen = false; editDraft = p.body; editOpen = true },
                                    )
                                }
                                DropdownMenuItem(
                                    text = { Text("Delete post") },
                                    onClick = { menuOpen = false; vm.deletePost(p.postId) },
                                )
                            } else {
                                if (foldOwner) {
                                    DropdownMenuItem(
                                        text = { Text("Remove from Fold") },
                                        onClick = { menuOpen = false; vm.deletePost(p.postId) },
                                    )
                                }
                                DropdownMenuItem(
                                    text = { Text("Report") },
                                    onClick = { menuOpen = false; reportOpen = true },
                                )
                                DropdownMenuItem(
                                    text = { Text("Block @${p.author.handle}") },
                                    onClick = { menuOpen = false; vm.block(p.author) },
                                )
                            }
                            if (!folded) DropdownMenuItem(
                                text = { Text(if (p.saved) "Remove from saved" else "Save post") },
                                onClick = { menuOpen = false; vm.toggleSavePost(p) },
                            )
                            if (!folded) DropdownMenuItem(
                                text = { Text("Share") },
                                onClick = {
                                    menuOpen = false
                                    val text = buildString {
                                        append(p.author.label)
                                        append(" on Nexus Connect")
                                        if (!warningRevealed) {
                                            append("\n\nContent warning: ").append(p.contentWarning)
                                        } else if (p.body.isNotBlank()) {
                                            append("\n\n").append(p.body)
                                        }
                                    }
                                    context.startActivity(
                                        Intent.createChooser(
                                            Intent(Intent.ACTION_SEND).apply {
                                                type = "text/plain"
                                                putExtra(Intent.EXTRA_TEXT, text)
                                            },
                                            "Share post",
                                        ),
                                    )
                                },
                            )
                            if (!folded && p.edited != null) {
                                DropdownMenuItem(
                                    text = { Text("Edit history") },
                                    onClick = { menuOpen = false; vm.loadPostRevisions(p.postId) },
                                )
                            }
                        }
                    }
                }
            }

            if (!warningRevealed) {
                Surface(
                    color = PanelHi,
                    shape = RoundedCornerShape(6.dp),
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 12.dp, vertical = 6.dp)
                        .clickable { warningRevealed = true },
                ) {
                    Column(Modifier.padding(14.dp)) {
                        Text("Content warning", color = Danger, fontWeight = FontWeight.Bold)
                        Text(p.contentWarning, color = TextDim, fontSize = 13.sp)
                        Spacer(Modifier.height(6.dp))
                        Text("Tap to reveal", color = Accent2, fontSize = 12.sp)
                    }
                }
            }

            if (warningRevealed && p.body.isNotEmpty()) {
                MentionText(
                    vm, p.body,
                    fontSize = 15.sp,
                    lineHeight = 21.sp,
                    modifier = Modifier.padding(horizontal = 14.dp, vertical = 4.dp),
                )
            }

            if (warningRevealed && p.media.isNotEmpty()) {
                MediaCarousel(vm, p)
            }

            // Reaction / comment bar. Tap = ❤️ toggle; long-press = the set.
            Row(
                Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box {
                    Row(
                        Modifier
                            .clip(RoundedCornerShape(8.dp))
                            .background(if (p.myReaction != null) AccentSoft else Color.Transparent)
                            .combinedClickable(
                                onClick = { vm.setReaction(p, p.myReaction ?: "❤️") },
                                onLongClick = { reactionsOpen = true },
                            )
                            .padding(horizontal = 10.dp, vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        if (p.myReaction != null) {
                            Text(p.myReaction, fontSize = 16.sp)
                        } else {
                            Text("♡", color = TextDim, fontSize = 17.sp)
                        }
                        Spacer(Modifier.width(6.dp))
                        Text(
                            if (p.reactions > 0) "${p.reactions}" else "Like",
                            color = if (p.myReaction != null) Accent2 else TextDim,
                            fontSize = 14.sp,
                            fontWeight = FontWeight.SemiBold,
                        )
                    }
                    DropdownMenu(
                        expanded = reactionsOpen,
                        onDismissRequest = { reactionsOpen = false },
                    ) {
                        Row(Modifier.padding(horizontal = 8.dp)) {
                            REACTIONS.forEach { r ->
                                Text(
                                    r, fontSize = 24.sp,
                                    modifier = Modifier
                                        .clickable {
                                            reactionsOpen = false
                                            vm.setReaction(p, r)
                                        }
                                        .padding(6.dp),
                                )
                            }
                        }
                    }
                }
                Row(
                    Modifier
                        .clip(RoundedCornerShape(8.dp))
                        .clickable { vm.loadComments(p.postId) }
                        .padding(horizontal = 10.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("💬", fontSize = 15.sp)
                    Spacer(Modifier.width(6.dp))
                    Text(
                        if (p.comments > 0) "${p.comments}" else "Comment",
                        color = TextDim, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
                    )
                }
            }
        }
    }

    if (editOpen) {
        AlertDialog(
            onDismissRequest = { editOpen = false },
            containerColor = Panel,
            title = { Text("Edit post", color = TextMain) },
            text = {
                OutlinedTextField(
                    value = editDraft, onValueChange = { editDraft = it },
                    minLines = 3, modifier = Modifier.fillMaxWidth(),
                )
            },
            confirmButton = {
                TextButton(
                    onClick = { editOpen = false; vm.editPost(p.postId, editDraft) },
                    enabled = editDraft.isNotBlank(),
                ) { Text("Save", color = Accent2) }
            },
            dismissButton = {
                TextButton(onClick = { editOpen = false }) { Text("Cancel", color = TextDim) }
            },
        )
    }

    if (reportOpen) {
        ReportDialog(
            onDismiss = { reportOpen = false },
            onSubmit = { reason -> reportOpen = false; vm.report(p, reason) },
        )
    }

    if (vm.revisionsPostId == p.postId) {
        AlertDialog(
            onDismissRequest = vm::closePostRevisions,
            containerColor = Panel,
            title = { Text("Edit history", color = TextMain) },
            text = {
                LazyColumn(Modifier.heightIn(max = 360.dp)) {
                    if (vm.postRevisions.isEmpty()) {
                        item { Text("No earlier versions.", color = TextDim) }
                    }
                    items(vm.postRevisions) { revision ->
                        Column(Modifier.padding(vertical = 8.dp)) {
                            Text(timeAgo(revision.replacedAt), color = TextMuted, fontSize = 11.sp)
                            Text(revision.body, color = TextMain, fontSize = 14.sp)
                        }
                        HorizontalDivider(color = BorderSoft)
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = vm::closePostRevisions) { Text("Close", color = Accent2) }
            },
        )
    }
}

/** Report a post: pick a reason so the moderation queue is reviewable. */
@Composable
fun ReportDialog(onDismiss: () -> Unit, onSubmit: (String) -> Unit) {
    var selected by remember { mutableStateOf<String?>(null) }
    var detail by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = Panel,
        title = { Text("Report post", color = TextMain) },
        text = {
            Column {
                Text(
                    "Why are you reporting this? Reports go to the server operator.",
                    color = TextDim, fontSize = 13.sp,
                )
                Spacer(Modifier.height(8.dp))
                REPORT_REASONS.forEach { r ->
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(8.dp))
                            .clickable { selected = r }
                            .padding(vertical = 2.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(selected = selected == r, onClick = { selected = r })
                        Text(r, color = TextMain, fontSize = 14.sp)
                    }
                }
                if (selected == "Other") {
                    OutlinedTextField(
                        value = detail, onValueChange = { detail = it },
                        label = { Text("Tell us more", color = TextDim) },
                        maxLines = 3, modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    val r = selected ?: return@TextButton
                    onSubmit(if (r == "Other" && detail.isNotBlank()) "Other: " + detail.trim() else r)
                },
                enabled = selected != null && (selected != "Other" || detail.isNotBlank()),
            ) { Text("Report", color = Danger) }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Cancel", color = TextDim) }
        },
    )
}

/** Swipeable photo carousel with page dots. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun MediaCarousel(vm: ConnectViewModel, post: Post) {
    val api = vm.api ?: return
    val state = rememberPagerState(pageCount = { post.media.size })
    Column {
        HorizontalPager(state = state, modifier = Modifier.fillMaxWidth()) { page ->
            val blobId = post.media[page]
            val mime = post.mediaTypes.getOrNull(page) ?: "image/jpeg"
            val folded = post.audience.startsWith("circle:")
            if (folded) LaunchedEffect(blobId) { vm.loadFoldMedia(post, blobId, mime) }
            if (mime.startsWith("video/")) {
                if (!folded) LaunchedEffect(blobId) { vm.loadPostVideo(blobId) }
                (if (folded) vm.foldMediaFiles[blobId] else vm.postVideoFiles[blobId])?.let { file ->
                    AndroidView(
                        factory = { context ->
                            VideoView(context).apply {
                                setVideoPath(file.absolutePath)
                                setMediaController(MediaController(context).also { it.setAnchorView(this) })
                            }
                        },
                        modifier = Modifier
                            .fillMaxWidth()
                            .aspectRatio(16f / 9f)
                            .padding(horizontal = 8.dp, vertical = 4.dp)
                            .clip(RoundedCornerShape(10.dp)),
                    )
                } ?: Box(
                    Modifier.fillMaxWidth().aspectRatio(16f / 9f),
                    contentAlignment = Alignment.Center,
                ) { CircularProgressIndicator(color = Accent2) }
            } else {
                AsyncImage(
                    model = if (folded) vm.foldMediaFiles[blobId] else api.mediaUrl(blobId),
                    contentDescription = post.altText.ifBlank { null },
                    contentScale = ContentScale.FillWidth,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 8.dp, vertical = 4.dp)
                        .clip(RoundedCornerShape(10.dp)),
                )
            }
        }
        if (post.media.size > 1) {
            Row(
                Modifier.fillMaxWidth().padding(top = 4.dp),
                horizontalArrangement = Arrangement.Center,
            ) {
                repeat(post.media.size) { i ->
                    Box(
                        Modifier
                            .padding(3.dp)
                            .size(6.dp)
                            .clip(CircleShape)
                            .background(if (i == state.currentPage) Accent2 else PanelHover),
                    )
                }
            }
        }
    }
}

// -------------------------------------------------------------- comments

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CommentsSheet(vm: ConnectViewModel, postId: String) {
    var draft by remember { mutableStateOf("") }
    var replyTo by remember { mutableStateOf<Comment?>(null) }
    val roots = vm.openComments.filter { it.parentId == null }
    val replies = vm.openComments.filter { it.parentId != null }.groupBy { it.parentId!! }
    ModalBottomSheet(
        onDismissRequest = { vm.closeComments() },
        containerColor = Panel,
    ) {
        Column(Modifier.fillMaxWidth().padding(bottom = 24.dp)) {
            Text(
                "Comments",
                color = TextMain,
                fontWeight = FontWeight.Bold,
                fontSize = 17.sp,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
            LazyColumn(Modifier.weight(1f, fill = false).heightIn(max = 420.dp)) {
                if (vm.openComments.isEmpty()) {
                    item {
                        Text(
                            "No comments yet — say something.",
                            color = TextDim,
                            modifier = Modifier.padding(16.dp),
                        )
                    }
                }
                roots.forEach { c ->
                    item(key = c.commentId) {
                        CommentRow(vm, c, indent = 0.dp, onReply = { replyTo = c })
                    }
                    replies[c.commentId].orEmpty().forEach { r ->
                        item(key = r.commentId) {
                            // Replies to a reply land under the same root.
                            CommentRow(vm, r, indent = 42.dp, onReply = { replyTo = c })
                        }
                    }
                }
            }
            if (replyTo != null) {
                Row(
                    Modifier
                        .fillMaxWidth()
                        .background(PanelHi)
                        .padding(horizontal = 16.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        "Replying to ${replyTo!!.author.label}",
                        color = Accent2, fontSize = 12.sp, modifier = Modifier.weight(1f),
                    )
                    Text(
                        "✕", color = TextDim, fontSize = 14.sp,
                        modifier = Modifier
                            .clip(CircleShape)
                            .clickable { replyTo = null }
                            .padding(horizontal = 6.dp, vertical = 2.dp),
                    )
                }
            }
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = { draft = it },
                    placeholder = {
                        Text(if (replyTo != null) "Write a reply…" else "Add a comment…")
                    },
                    modifier = Modifier.weight(1f),
                    maxLines = 3,
                )
                Spacer(Modifier.width(8.dp))
                FilledIconButton(
                    onClick = {
                        vm.addComment(postId, draft, replyTo?.commentId) {
                            draft = ""
                            replyTo = null
                        }
                    },
                    enabled = draft.isNotBlank() && !vm.busy,
                ) { Text("➤") }
            }
        }
    }
}

/** One comment (or indented reply) with like + reply actions. */
@Composable
private fun CommentRow(
    vm: ConnectViewModel,
    c: Comment,
    indent: androidx.compose.ui.unit.Dp,
    onReply: () -> Unit,
) {
    var editOpen by remember { mutableStateOf(false) }
    var editDraft by remember { mutableStateOf("") }
    var deleteOpen by remember { mutableStateOf(false) }
    var reportOpen by remember { mutableStateOf(false) }
    Row(Modifier.padding(start = 16.dp + indent, end = 16.dp, top = 8.dp, bottom = 2.dp)) {
        AuthorAvatar(vm, c.author, if (indent > 0.dp) 26.dp else 32.dp)
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    c.author.label, color = TextMain,
                    fontWeight = FontWeight.SemiBold, fontSize = 14.sp,
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    timeAgo(c.created) + if (c.edited != null) " · edited" else "",
                    color = TextMuted, fontSize = 12.sp,
                )
            }
            MentionText(vm, c.body, fontSize = 14.sp)
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (c.myReaction != null) "❤️" else "♡",
                    color = if (c.myReaction != null) Accent2 else TextMuted,
                    fontSize = 13.sp,
                    modifier = Modifier
                        .clip(RoundedCornerShape(6.dp))
                        .clickable { vm.toggleCommentLike(c) }
                        .padding(horizontal = 6.dp, vertical = 3.dp),
                )
                if (c.reactions > 0) {
                    Text(
                        "${c.reactions}",
                        color = if (c.myReaction != null) Accent2 else TextMuted,
                        fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                    )
                }
                Spacer(Modifier.width(12.dp))
                Text(
                    "Reply", color = TextMuted, fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .clip(RoundedCornerShape(6.dp))
                        .clickable(onClick = onReply)
                        .padding(horizontal = 6.dp, vertical = 3.dp),
                )
                if (c.author.accountId == vm.accountId) {
                    Spacer(Modifier.width(12.dp))
                    Text(
                        "Edit", color = TextMuted, fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier
                            .clip(RoundedCornerShape(6.dp))
                            .clickable { editDraft = c.body; editOpen = true }
                            .padding(horizontal = 6.dp, vertical = 3.dp),
                    )
                }
                if (c.author.accountId == vm.accountId || vm.detailPost?.author?.accountId == vm.accountId) {
                    Spacer(Modifier.width(12.dp))
                    Text(
                        "Delete", color = Danger, fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier
                            .clip(RoundedCornerShape(6.dp))
                            .clickable { deleteOpen = true }
                            .padding(horizontal = 6.dp, vertical = 3.dp),
                    )
                }
                if (c.author.accountId != vm.accountId) {
                    Spacer(Modifier.width(12.dp))
                    Text(
                        "Report", color = TextMuted, fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier
                            .clip(RoundedCornerShape(6.dp))
                            .clickable { reportOpen = true }
                            .padding(horizontal = 6.dp, vertical = 3.dp),
                    )
                }
            }
        }
    }

    if (editOpen) {
        AlertDialog(
            onDismissRequest = { editOpen = false },
            containerColor = Panel,
            title = { Text("Edit comment", color = TextMain) },
            text = {
                OutlinedTextField(
                    value = editDraft, onValueChange = { editDraft = it },
                    minLines = 2, modifier = Modifier.fillMaxWidth(),
                )
            },
            confirmButton = {
                TextButton(
                    onClick = { editOpen = false; vm.editComment(c, editDraft) },
                    enabled = editDraft.isNotBlank(),
                ) { Text("Save", color = Accent2) }
            },
            dismissButton = {
                TextButton(onClick = { editOpen = false }) { Text("Cancel", color = TextDim) }
            },
        )
    }

    if (deleteOpen) {
        AlertDialog(
            onDismissRequest = { deleteOpen = false },
            containerColor = Panel,
            title = { Text("Delete comment?", color = TextMain) },
            text = { Text("This comment will be removed permanently.", color = TextDim) },
            confirmButton = {
                TextButton(onClick = { deleteOpen = false; vm.deleteComment(c) }) {
                    Text("Delete", color = Danger)
                }
            },
            dismissButton = {
                TextButton(onClick = { deleteOpen = false }) { Text("Cancel", color = TextDim) }
            },
        )
    }
    if (reportOpen) {
        ReportDialog(
            onDismiss = { reportOpen = false },
            onSubmit = { reason -> reportOpen = false; vm.reportComment(c, reason) },
        )
    }
}
