// Owns feature panes, dialogs, and user actions inside the Connect shell.
// It does not perform HIVE requests or persist records directly; all such work
// is delegated to ConnectViewModel.

package com.nexusway.connect

import android.Manifest
import android.content.Context
import android.media.MediaPlayer
import android.net.Uri
import android.widget.MediaController
import android.widget.VideoView
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.ExitToApp
import androidx.compose.material.icons.automirrored.outlined.Send
import androidx.compose.material.icons.filled.Hexagon
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.BookmarkBorder
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.Campaign
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Group
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.Hexagon
import androidx.compose.material.icons.outlined.MailOutline
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Movie
import androidx.compose.material.icons.outlined.Pause
import androidx.compose.material.icons.outlined.PhotoCamera
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material.icons.outlined.Security
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Storage
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material.icons.outlined.VpnKey
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import coil.compose.AsyncImage
import java.io.File

/** A fresh content: Uri under cache/camera/ the system camera can write to. */
private fun newCaptureUri(context: Context): Uri {
    val dir = File(context.cacheDir, "camera").apply { mkdirs() }
    val file = File.createTempFile("capture_", ".jpg", dir)
    return FileProvider.getUriForFile(
        context, "${context.packageName}.fileprovider", file,
    )
}

private fun newVideoCaptureUri(context: Context): Uri {
    val dir = File(context.cacheDir, "camera").apply { mkdirs() }
    val file = File.createTempFile("capture_", ".mp4", dir)
    return FileProvider.getUriForFile(
        context, "${context.packageName}.fileprovider", file,
    )
}

@Composable
private fun HexHeaderAction(
    icon: ImageVector,
    description: String,
    onClick: () -> Unit,
) {
    Box(
        Modifier.size(48.dp).clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            Icons.Filled.Hexagon,
            null,
            tint = Color(0xFF102A56),
            modifier = Modifier.size(46.dp),
        )
        Icon(
            Icons.Outlined.Hexagon,
            null,
            tint = Color.White,
            modifier = Modifier.size(46.dp),
        )
        Icon(icon, description, tint = Color.White, modifier = Modifier.size(21.dp))
    }
}

// -------------------------------------------------------------------- home

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FeedPane(vm: ConnectViewModel) {
    var searchOpen by remember { mutableStateOf(false) }
    var postQuery by remember { mutableStateOf("") }
    Column(Modifier.fillMaxSize()) {
        // Header: matching Messages/Folds hex actions around the centered wordmark.
        Box(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp)) {
            Box(Modifier.align(Alignment.CenterStart)) {
                HexHeaderAction(Icons.AutoMirrored.Outlined.Send, "Messages") {
                    vm.dmOpen = true
                }
            }
            Text(
                "CONNECT",
                style = BrandTextStyle, fontSize = 20.sp,
                fontWeight = FontWeight.Black, letterSpacing = 3.sp,
                modifier = Modifier.align(Alignment.Center),
            )
            if (vm.busy) {
                CircularProgressIndicator(
                    color = Accent2, strokeWidth = 2.dp,
                    modifier = Modifier.size(18.dp).align(Alignment.CenterEnd).offset(x = (-52).dp),
                )
            }
            Box(Modifier.align(Alignment.CenterEnd)) {
                HexHeaderAction(Icons.Outlined.Group, "Folds") { vm.foldsOpen = true }
            }
        }
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.Center,
        ) {
            IconButton(onClick = { searchOpen = false; vm.showHomeFeed() }) {
                Icon(
                    Icons.Outlined.Home,
                    "Home feed",
                    tint = if (vm.feedMode == "home") Accent2 else TextMuted,
                )
            }
            IconButton(onClick = { searchOpen = false; vm.showSavedPosts() }) {
                Icon(
                    Icons.Outlined.BookmarkBorder,
                    "Saved posts",
                    tint = if (vm.feedMode == "saved") Accent2 else TextMuted,
                )
            }
            IconButton(onClick = { searchOpen = !searchOpen }) {
                Icon(
                    Icons.Outlined.Search,
                    "Search posts",
                    tint = if (vm.feedMode == "search" || searchOpen) Accent2 else TextMuted,
                )
            }
            Text(
                when (vm.feedMode) {
                    "saved" -> "SAVED"
                    "search" -> "SEARCH RESULTS"
                    else -> "LATEST"
                },
                color = TextMuted,
                fontSize = 11.sp,
                fontWeight = FontWeight.Bold,
            )
        }
        if (searchOpen) {
            OutlinedTextField(
                value = postQuery,
                onValueChange = { postQuery = it.take(100) },
                placeholder = { Text("Search posts or #hashtags") },
                singleLine = true,
                trailingIcon = {
                    IconButton(
                        onClick = { vm.searchPosts(postQuery); searchOpen = false },
                        enabled = postQuery.isNotBlank(),
                    ) { Icon(Icons.Outlined.Search, "Run search", tint = Accent2) }
                },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
        if (vm.status.isNotEmpty()) {
            Text(
                vm.status, color = Accent2, fontSize = 13.sp,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
        }
        PullToRefreshBox(
            isRefreshing = vm.busy,
            onRefresh = { vm.refresh() },
            modifier = Modifier.fillMaxSize(),
        ) {
            LazyColumn(Modifier.fillMaxSize()) {
                if (vm.posts.isEmpty() && !vm.busy) {
                    item {
                        Column(
                            Modifier.fillParentMaxSize().padding(32.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.Center,
                        ) {
                            Text(
                                when (vm.feedMode) {
                                    "saved" -> "No saved posts"
                                    "search" -> "No matching posts"
                                    else -> "Your feed is quiet"
                                },
                                color = TextMain,
                                fontSize = 17.sp,
                            )
                            Spacer(Modifier.height(8.dp))
                            Text(
                                when (vm.feedMode) {
                                    "saved" -> "Save a post from its menu and it will appear here."
                                    "search" -> "Try another phrase or hashtag."
                                    else -> "Find people in Discover, or make the first post with +."
                                },
                                color = TextDim, fontSize = 14.sp, textAlign = TextAlign.Center)
                        }
                    }
                } else {
                    items(vm.posts, key = { it.postId }) { p -> PostCard(vm, p) }
                    if (vm.nextCursor != null) {
                        item {
                            TextButton(
                                onClick = { vm.loadMore() },
                                modifier = Modifier.fillMaxWidth().padding(8.dp),
                            ) { Text("Load older posts", color = TextDim) }
                        }
                    }
                    item { Spacer(Modifier.height(60.dp)) }
                }
            }
        }
    }
}

// ------------------------------------------------------------------- folds

@Composable
fun FoldsPane(vm: ConnectViewModel) {
    var ownedExpanded by remember { mutableStateOf(true) }
    var joinedExpanded by remember { mutableStateOf(true) }
    var creating by remember { mutableStateOf(false) }
    var newFoldName by remember { mutableStateOf("") }
    var manageFoldId by remember { mutableStateOf<String?>(null) }
    var selectedFoldId by remember { mutableStateOf<String?>(null) }
    val owned = vm.folds.filter { it.owned }
    val joined = vm.folds.filter { !it.owned && !it.invited }
    val invitations = vm.folds.filter { it.invited }
    val selectedFold = vm.folds.find { it.circleId == selectedFoldId }

    LaunchedEffect(Unit) { vm.refreshFolds() }
    LaunchedEffect(vm.requestedFoldId, vm.folds) {
        vm.requestedFoldId?.let { requestedId ->
            vm.folds.firstOrNull { it.circleId == requestedId && !it.invited }?.let {
                selectedFoldId = requestedId
                vm.consumeRequestedFold()
            }
        }
    }
    BackHandler {
        if (selectedFoldId != null) selectedFoldId = null else vm.foldsOpen = false
    }

    if (selectedFold != null) {
        FoldDetailPane(
            vm = vm,
            fold = selectedFold,
            onBack = { selectedFoldId = null },
            onManage = if (selectedFold.owned) {
                { manageFoldId = selectedFold.circleId }
            } else {
                null
            },
        )
    } else Box(Modifier.fillMaxSize()) {
        HiveHoneycombBackground()
        Column(Modifier.fillMaxSize()) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(onClick = { vm.foldsOpen = false }) {
                    Text("← Back", color = TextDim)
                }
                Spacer(Modifier.weight(1f))
                Text(
                    "FOLDS",
                    style = BrandTextStyle,
                    fontSize = 17.sp,
                    fontWeight = FontWeight.Black,
                    letterSpacing = 3.sp,
                )
                Spacer(Modifier.weight(1f))
                IconButton(onClick = { creating = !creating }) {
                    Icon(Icons.Outlined.Add, "Create Fold", tint = Accent2)
                }
            }
            HorizontalDivider(color = BorderSoft)
            LazyColumn(
                Modifier.fillMaxSize(),
                contentPadding = PaddingValues(horizontal = 14.dp, vertical = 14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (invitations.isNotEmpty()) {
                    item {
                        Text(
                            "FOLD INVITATIONS",
                            color = Honey,
                            fontSize = 12.sp,
                            fontWeight = FontWeight.Bold,
                            letterSpacing = 1.sp,
                        )
                    }
                    items(invitations, key = { "invite-${it.circleId}" }) { fold ->
                        Surface(
                            color = Panel,
                            shape = HiveCutShape,
                            border = BorderStroke(1.dp, Honey.copy(alpha = 0.5f)),
                            modifier = Modifier.fillMaxWidth().hivePanelDepth(),
                        ) {
                            Column(Modifier.padding(14.dp)) {
                                Text(fold.name, color = TextMain, fontWeight = FontWeight.Bold)
                                Text(
                                    "Invited by ${fold.ownerLabel.ifEmpty { "the Fold owner" }}",
                                    color = TextMuted,
                                    fontSize = 12.sp,
                                )
                                Row(
                                    Modifier.fillMaxWidth().padding(top = 10.dp),
                                    horizontalArrangement = Arrangement.End,
                                ) {
                                    TextButton(onClick = { vm.foldDecline(fold) }) {
                                        Text("Decline", color = TextDim)
                                    }
                                    Button(onClick = { vm.foldAccept(fold) }) { Text("Accept") }
                                }
                            }
                        }
                    }
                }
                if (creating) {
                    item {
                        Surface(
                            color = Panel,
                            shape = HiveCutShape,
                            border = BorderStroke(1.dp, Accent2.copy(alpha = 0.45f)),
                            modifier = Modifier.fillMaxWidth().hivePanelDepth(),
                        ) {
                            Column(Modifier.padding(14.dp)) {
                                Text(
                                    "Create a Fold",
                                    color = TextMain,
                                    fontWeight = FontWeight.Bold,
                                    fontSize = 16.sp,
                                )
                                Text(
                                    "Start a private member space. Encrypted posting is coming next.",
                                    color = TextMuted,
                                    fontSize = 12.sp,
                                    modifier = Modifier.padding(top = 3.dp, bottom = 10.dp),
                                )
                                OutlinedTextField(
                                    value = newFoldName,
                                    onValueChange = { newFoldName = it.take(64) },
                                    label = { Text("Fold name") },
                                    singleLine = true,
                                    modifier = Modifier.fillMaxWidth(),
                                )
                                Row(
                                    Modifier.fillMaxWidth().padding(top = 10.dp),
                                    horizontalArrangement = Arrangement.End,
                                ) {
                                    TextButton(onClick = {
                                        creating = false
                                        newFoldName = ""
                                    }) { Text("Cancel", color = TextDim) }
                                    Spacer(Modifier.width(6.dp))
                                    Button(
                                        onClick = {
                                            vm.createFold(newFoldName)
                                            creating = false
                                            newFoldName = ""
                                        },
                                        enabled = newFoldName.isNotBlank() && !vm.busy,
                                    ) { Text("Create") }
                                }
                            }
                        }
                    }
                }

                item {
                    FoldSectionHeader(
                        title = "Owned Folds",
                        count = owned.size,
                        expanded = ownedExpanded,
                        onClick = { ownedExpanded = !ownedExpanded },
                    )
                }
                if (ownedExpanded) {
                    if (owned.isEmpty()) {
                        item { FoldEmptyRow("You haven't created a Fold yet.") }
                    } else {
                        items(owned, key = { "owned-${it.circleId}" }) { fold ->
                            FoldRow(
                                fold = fold,
                                detail = "${fold.members.size} active members · You own this Fold",
                                onClick = { selectedFoldId = fold.circleId },
                            )
                        }
                    }
                }

                item {
                    FoldSectionHeader(
                        title = "Joined Folds",
                        count = joined.size,
                        expanded = joinedExpanded,
                        onClick = { joinedExpanded = !joinedExpanded },
                    )
                }
                if (joinedExpanded) {
                    if (joined.isEmpty()) {
                        item { FoldEmptyRow("Folds you join will appear here.") }
                    } else {
                        items(joined, key = { "joined-${it.circleId}" }) { fold ->
                            FoldRow(
                                fold = fold,
                                detail = "Owned by ${fold.ownerLabel.ifEmpty { "another member" }}",
                                onClick = { selectedFoldId = fold.circleId },
                            )
                        }
                    }
                }
                item { Spacer(Modifier.height(24.dp)) }
            }
        }
    }

    manageFoldId?.let { id ->
        FoldManageSheet(vm, id, onClose = { manageFoldId = null })
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FoldDetailPane(
    vm: ConnectViewModel,
    fold: Fold,
    onBack: () -> Unit,
    onManage: (() -> Unit)?,
) {
    var composerOpen by remember { mutableStateOf(false) }
    var draft by remember { mutableStateOf("") }
    var attachments by remember { mutableStateOf(listOf<PendingPostMedia>()) }
    var altText by remember { mutableStateOf("") }
    var warning by remember { mutableStateOf("") }
    var confirmLeave by remember { mutableStateOf(false) }
    val foldReady = vm.foldCanPost(fold)
    val context = LocalContext.current
    val foldImages = rememberLauncherForActivityResult(
        ActivityResultContracts.PickMultipleVisualMedia(10),
    ) { uris ->
        if (uris.isNotEmpty()) attachments = uris.map { PendingPostMedia(it, "image/jpeg") }
    }
    val foldVideo = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            val mime = context.contentResolver.getType(uri)?.takeIf { it.startsWith("video/") }
                ?: "video/mp4"
            attachments = listOf(PendingPostMedia(uri, mime))
        }
    }
    LaunchedEffect(fold.circleId, fold.keyEpoch, foldReady) {
        if (foldReady) vm.refreshFoldFeed(fold)
    }

    Box(Modifier.fillMaxSize()) {
        HiveHoneycombBackground()
        Column(Modifier.fillMaxSize()) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(onClick = onBack) { Text("← Folds", color = TextDim) }
                Spacer(Modifier.weight(1f))
                Text(
                    fold.name.uppercase(),
                    style = BrandTextStyle,
                    fontSize = 16.sp,
                    fontWeight = FontWeight.Black,
                    maxLines = 1,
                )
                Spacer(Modifier.weight(1f))
                if (onManage != null) {
                    IconButton(onClick = onManage) {
                        Icon(Icons.Outlined.Settings, "Manage ${fold.name}", tint = Accent2)
                    }
                } else {
                    IconButton(onClick = { confirmLeave = true }) {
                        Icon(Icons.AutoMirrored.Outlined.ExitToApp, "Leave ${fold.name}", tint = Danger)
                    }
                }
            }
            HorizontalDivider(color = BorderSoft)
            PullToRefreshBox(
                isRefreshing = vm.busy,
                onRefresh = { vm.refreshFoldFeed(fold) },
                modifier = Modifier.fillMaxSize(),
            ) {
                LazyColumn(
                    Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(vertical = 16.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                item {
                    Surface(
                        color = Panel,
                        shape = HiveCutShape,
                        border = BorderStroke(1.dp, Accent2.copy(alpha = 0.35f)),
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp).hivePanelDepth(),
                    ) {
                        Row(
                            Modifier.padding(16.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Box(
                                Modifier.size(52.dp).clip(RoundedCornerShape(8.dp))
                                    .background(Color(0xFF102A56)),
                                contentAlignment = Alignment.Center,
                            ) {
                                Icon(
                                    Icons.Outlined.Lock,
                                    null,
                                    tint = Color.White,
                                    modifier = Modifier.size(25.dp),
                                )
                            }
                            Spacer(Modifier.width(13.dp))
                            Column(Modifier.weight(1f)) {
                                Text(
                                    "Private Fold",
                                    color = Accent2,
                                    fontWeight = FontWeight.Bold,
                                    fontSize = 12.sp,
                                )
                                Text(
                                    if (fold.owned) {
                                        "${fold.members.size} active members · You own this Fold"
                                    } else {
                                        "${fold.members.size} active members · Owned by ${fold.ownerLabel.ifEmpty { "another member" }}"
                                    },
                                    color = TextMain,
                                    fontSize = 14.sp,
                                )
                                Text(
                                    if (fold.owned) "Manage members from the settings action above."
                                    else "Your membership is active.",
                                    color = TextMuted,
                                    fontSize = 12.sp,
                                )
                            }
                        }
                    }
                }
                item {
                    LazyRow(
                        contentPadding = PaddingValues(horizontal = 14.dp),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        items(fold.members, key = { "roster-${it.accountId}" }) { member ->
                            Column(
                                horizontalAlignment = Alignment.CenterHorizontally,
                                modifier = Modifier.width(64.dp).clickable {
                                    vm.openProfile(member.accountId)
                                },
                            ) {
                                AuthorAvatar(vm, member, 42.dp)
                                Text(
                                    member.handle,
                                    color = TextMuted,
                                    fontSize = 11.sp,
                                    maxLines = 1,
                                )
                            }
                        }
                    }
                }
                item {
                    Button(
                        onClick = { composerOpen = true },
                        enabled = foldReady,
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp).height(46.dp),
                    ) {
                        Text(
                            when {
                                foldReady -> "Post to ${fold.name}"
                                !vm.encryptedFoldsReady -> "Encrypted activity pending HIVE update"
                                else -> "Secure member keys are not ready"
                            },
                        )
                    }
                }
                item {
                    Text(
                        "FOLD ACTIVITY",
                        color = Accent2,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Bold,
                        letterSpacing = 1.sp,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                    )
                }
                if (!foldReady) {
                    item {
                        Text(
                            if (!vm.encryptedFoldsReady) {
                                "The private Fold feed, member publishing, and invitation acceptance are ready in this app and will activate with the pending HIVE update."
                            } else {
                                "Encrypted activity is paused until every active member has opened Connect and the owner refreshes this Fold's device keys."
                            },
                            color = TextMuted,
                            fontSize = 14.sp,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.fillMaxWidth().padding(32.dp),
                        )
                    }
                } else if (vm.foldPosts.isEmpty() && !vm.busy) {
                    item {
                        Text(
                            "No Fold posts yet. Every active member can start the conversation.",
                            color = TextMuted,
                            fontSize = 14.sp,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.fillMaxWidth().padding(32.dp),
                        )
                    }
                } else {
                    items(vm.foldPosts, key = { "fold-post-${it.postId}" }) { post ->
                        PostCard(vm, post)
                    }
                    if (vm.foldNextCursor != null) {
                        item {
                            TextButton(
                                onClick = { vm.loadMoreFoldPosts(fold) },
                                modifier = Modifier.fillMaxWidth(),
                            ) { Text("Load older Fold posts", color = TextDim) }
                        }
                    }
                }
                item { Spacer(Modifier.height(24.dp)) }
                }
            }
        }
    }

    if (composerOpen) {
        ModalBottomSheet(onDismissRequest = { composerOpen = false }, containerColor = Panel) {
            Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp)) {
                Text(
                    "Post to ${fold.name}",
                    color = TextMain,
                    fontSize = 18.sp,
                    fontWeight = FontWeight.Bold,
                )
                Text(
                    "End-to-end encrypted for active Fold members",
                    color = Accent2,
                    fontSize = 12.sp,
                )
                OutlinedTextField(
                    value = draft,
                    onValueChange = { draft = it.take(10_000) },
                    placeholder = { Text("Share with the Fold") },
                    minLines = 5,
                    maxLines = 10,
                    modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
                )
                OutlinedTextField(
                    value = warning,
                    onValueChange = { warning = it.take(200) },
                    label = { Text("Content warning (optional)") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                )
                Row(
                    Modifier.fillMaxWidth().padding(top = 8.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    OutlinedButton(
                        onClick = {
                            foldImages.launch(
                                PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                            )
                        },
                        modifier = Modifier.weight(1f),
                    ) {
                        Icon(Icons.Outlined.Image, null)
                        Spacer(Modifier.width(6.dp))
                        Text("Photos")
                    }
                    OutlinedButton(
                        onClick = {
                            foldVideo.launch(
                                PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.VideoOnly),
                            )
                        },
                        modifier = Modifier.weight(1f),
                    ) {
                        Icon(Icons.Outlined.Movie, null)
                        Spacer(Modifier.width(6.dp))
                        Text("Video")
                    }
                }
                if (attachments.isNotEmpty()) {
                    Text(
                        "${attachments.size} encrypted attachment${if (attachments.size == 1) "" else "s"} selected",
                        color = Accent2,
                        fontSize = 12.sp,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                    OutlinedTextField(
                        value = altText,
                        onValueChange = { altText = it.take(2_000) },
                        label = { Text("Alt text") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                    )
                }
                Button(
                    onClick = {
                        vm.createFoldPost(fold, draft, attachments, altText, warning) {
                            draft = ""
                            attachments = emptyList()
                            altText = ""
                            warning = ""
                            composerOpen = false
                        }
                    },
                    enabled = (draft.isNotBlank() || attachments.isNotEmpty()) && !vm.busy,
                    modifier = Modifier.fillMaxWidth().padding(vertical = 16.dp),
                ) { Text("Post securely") }
            }
        }
    }

    if (confirmLeave) {
        AlertDialog(
            onDismissRequest = { confirmLeave = false },
            title = { Text("Leave ${fold.name}?") },
            text = { Text("You will lose access immediately. The owner must rotate the Fold key before new posts can be created.") },
            confirmButton = {
                TextButton(onClick = {
                    confirmLeave = false
                    vm.foldLeave(fold)
                    onBack()
                }) { Text("Leave", color = Danger) }
            },
            dismissButton = {
                TextButton(onClick = { confirmLeave = false }) { Text("Cancel") }
            },
        )
    }
}

@Composable
private fun FoldSectionHeader(
    title: String,
    count: Int,
    expanded: Boolean,
    onClick: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(6.dp)).clickable(onClick = onClick)
            .background(PanelHi).padding(horizontal = 14.dp, vertical = 11.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, color = TextMain, fontWeight = FontWeight.Bold, fontSize = 15.sp)
        Spacer(Modifier.width(8.dp))
        Text("$count", color = Accent2, fontWeight = FontWeight.Bold, fontSize = 13.sp)
        Spacer(Modifier.weight(1f))
        Icon(
            if (expanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore,
            if (expanded) "Collapse $title" else "Expand $title",
            tint = TextDim,
        )
    }
}

@Composable
private fun FoldRow(fold: Fold, detail: String, onClick: () -> Unit) {
    Surface(
        onClick = onClick,
        color = Panel,
        shape = HiveCutShape,
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth().hivePanelDepth(),
    ) {
        Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
            Box(
                Modifier.size(42.dp).clip(RoundedCornerShape(7.dp)).background(Color(0xFF102A56)),
                contentAlignment = Alignment.Center,
            ) {
                Icon(Icons.Outlined.Lock, null, tint = Color.White, modifier = Modifier.size(21.dp))
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(fold.name, color = TextMain, fontWeight = FontWeight.SemiBold, fontSize = 15.sp)
                Text(detail, color = TextMuted, fontSize = 12.sp)
            }
            Icon(Icons.Outlined.ChevronRight, null, tint = TextDim)
        }
    }
}

@Composable
private fun FoldEmptyRow(message: String) {
    Text(
        message,
        color = TextMuted,
        fontSize = 13.sp,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 12.dp),
    )
}

// ---------------------------------------------------------------- support

@Composable
fun SupportPane(vm: ConnectViewModel) {
    var selectedId by remember { mutableStateOf<String?>(null) }
    var creating by remember { mutableStateOf(false) }
    var category by remember { mutableStateOf("bug") }
    var subject by remember { mutableStateOf("") }
    var body by remember { mutableStateOf("") }
    var reply by remember { mutableStateOf("") }
    val selected = vm.supportThreads.firstOrNull { it.id == selectedId }
    LaunchedEffect(vm.encryptedFoldsReady) {
        if (vm.encryptedFoldsReady) vm.refreshSupportThreads()
    }
    BackHandler {
        when {
            selectedId != null -> selectedId = null
            creating -> creating = false
            else -> vm.supportOpen = false
        }
    }

    Box(Modifier.fillMaxSize()) {
        HiveHoneycombBackground()
        Column(Modifier.fillMaxSize()) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(onClick = {
                    when {
                        selectedId != null -> selectedId = null
                        creating -> creating = false
                        else -> vm.supportOpen = false
                    }
                }) { Text("← Back", color = TextDim) }
                Spacer(Modifier.weight(1f))
                Text(
                    if (selected != null) "SUPPORT THREAD" else if (creating) "NEW REQUEST" else "SUPPORT",
                    style = BrandTextStyle,
                    fontSize = 16.sp,
                    fontWeight = FontWeight.Black,
                    letterSpacing = 2.sp,
                )
                Spacer(Modifier.weight(1f))
                if (selected == null && !creating && vm.encryptedFoldsReady) {
                    IconButton(onClick = { creating = true }) {
                        Icon(Icons.Outlined.Add, "New support request", tint = Honey)
                    }
                } else Spacer(Modifier.size(48.dp))
            }
            HorizontalDivider(color = BorderSoft)

            when {
                creating -> {
                    Column(
                        Modifier.fillMaxSize().verticalScroll(rememberScrollState())
                            .padding(horizontal = 18.dp, vertical = 16.dp),
                    ) {
                        Text("What can we help with?", color = TextMain, fontWeight = FontWeight.Bold)
                        Row(
                            Modifier.fillMaxWidth().horizontalScroll(rememberScrollState())
                                .padding(vertical = 10.dp),
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            listOf(
                                "bug" to "Bug",
                                "feature" to "Feature",
                                "help" to "Help",
                                "other" to "Other",
                            ).forEach { (value, label) ->
                                FilterChip(
                                    selected = category == value,
                                    onClick = { category = value },
                                    label = { Text(label) },
                                )
                            }
                        }
                        OutlinedTextField(
                            value = subject,
                            onValueChange = { subject = it.take(120) },
                            label = { Text("Subject") },
                            singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        OutlinedTextField(
                            value = body,
                            onValueChange = { body = it.take(4_000) },
                            label = { Text("Details") },
                            placeholder = { Text("Include what happened, what you expected, and steps to reproduce") },
                            minLines = 7,
                            maxLines = 14,
                            modifier = Modifier.fillMaxWidth().padding(top = 10.dp),
                        )
                        Text(
                            "Support messages are sent securely to the Nexus-Way Console and retained with your account so the team can reply.",
                            color = TextMuted,
                            fontSize = 12.sp,
                            modifier = Modifier.padding(top = 10.dp),
                        )
                        Button(
                            onClick = {
                                vm.openSupportThread(category, subject, body) {
                                    subject = ""
                                    body = ""
                                    creating = false
                                }
                            },
                            enabled = subject.isNotBlank() && body.isNotBlank() && !vm.busy,
                            modifier = Modifier.fillMaxWidth().padding(top = 16.dp),
                        ) { Text("Send to Support") }
                    }
                }
                selected != null -> {
                    LazyColumn(
                        Modifier.weight(1f).fillMaxWidth(),
                        contentPadding = PaddingValues(horizontal = 14.dp, vertical = 14.dp),
                        verticalArrangement = Arrangement.spacedBy(9.dp),
                    ) {
                        item {
                            Text(selected.subject, color = TextMain, fontWeight = FontWeight.Bold, fontSize = 17.sp)
                            Text(
                                "${selected.category.uppercase()} · ${selected.status.uppercase()}",
                                color = if (selected.status == "open") Honey else TextMuted,
                                fontSize = 11.sp,
                                fontWeight = FontWeight.Bold,
                            )
                        }
                        items(selected.messages, key = { it.id }) { message ->
                            Row(
                                Modifier.fillMaxWidth(),
                                horizontalArrangement = if (message.senderRole == "user") Arrangement.End else Arrangement.Start,
                            ) {
                                Surface(
                                    color = if (message.senderRole == "user") AccentSoft else PanelHi,
                                    shape = RoundedCornerShape(8.dp),
                                    border = BorderStroke(1.dp, if (message.senderRole == "admin") Honey.copy(alpha = 0.45f) else BorderSoft),
                                    modifier = Modifier.fillMaxWidth(0.86f),
                                ) {
                                    Column(Modifier.padding(12.dp)) {
                                        Text(
                                            if (message.senderRole == "admin") "Nexus-Way Support" else "You",
                                            color = if (message.senderRole == "admin") Honey else Accent2,
                                            fontSize = 11.sp,
                                            fontWeight = FontWeight.Bold,
                                        )
                                        Text(message.body, color = TextMain, fontSize = 14.sp)
                                        Text(timeAgo(message.created), color = TextMuted, fontSize = 10.sp)
                                    }
                                }
                            }
                        }
                    }
                    if (selected.status == "open") {
                        Row(
                            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            OutlinedTextField(
                                value = reply,
                                onValueChange = { reply = it.take(4_000) },
                                placeholder = { Text("Add more information") },
                                maxLines = 4,
                                modifier = Modifier.weight(1f),
                            )
                            IconButton(
                                onClick = {
                                    vm.sendSupportReply(selected.id, reply) { reply = "" }
                                },
                                enabled = reply.isNotBlank() && !vm.busy,
                            ) {
                                Icon(Icons.AutoMirrored.Outlined.Send, "Send support reply", tint = Honey)
                            }
                        }
                    }
                }
                else -> {
                    Button(
                        onClick = { creating = true },
                        enabled = vm.encryptedFoldsReady,
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 12.dp),
                    ) {
                        Text(
                            if (vm.encryptedFoldsReady) "Report a bug or request a feature"
                            else "Support messaging pending HIVE update",
                        )
                    }
                    LazyColumn(
                        Modifier.weight(1f).fillMaxWidth(),
                        contentPadding = PaddingValues(horizontal = 12.dp, vertical = 4.dp),
                        verticalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        if (!vm.encryptedFoldsReady) {
                            item {
                                Text(
                                    "The direct user-to-Console support inbox is ready in this app and will activate with the pending HIVE update.",
                                    color = TextMuted,
                                    textAlign = TextAlign.Center,
                                    modifier = Modifier.fillMaxWidth().padding(32.dp),
                                )
                            }
                        } else if (vm.supportThreads.isEmpty() && !vm.busy) {
                            item {
                                Text(
                                    "No support conversations yet.",
                                    color = TextMuted,
                                    textAlign = TextAlign.Center,
                                    modifier = Modifier.fillMaxWidth().padding(32.dp),
                                )
                            }
                        }
                        items(vm.supportThreads, key = { it.id }) { thread ->
                            Surface(
                                onClick = { selectedId = thread.id },
                                color = Panel,
                                shape = HiveCutShape,
                                border = BorderStroke(1.dp, if (thread.status == "open") Honey.copy(alpha = 0.35f) else BorderSoft),
                                modifier = Modifier.fillMaxWidth().hivePanelDepth(),
                            ) {
                                Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
                                    Column(Modifier.weight(1f)) {
                                        Text(thread.subject, color = TextMain, fontWeight = FontWeight.SemiBold)
                                        Text(
                                            "${thread.category.uppercase()} · ${thread.status} · ${timeAgo(thread.updated)}",
                                            color = TextMuted,
                                            fontSize = 12.sp,
                                        )
                                    }
                                    Icon(Icons.Outlined.ChevronRight, null, tint = TextDim)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- messages

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun DmPane(vm: ConnectViewModel) {
    val context = LocalContext.current
    var selectedAccount by remember { mutableStateOf<String?>(null) }
    var creating by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }
    var draft by remember { mutableStateOf("") }
    var selectedAttachment by remember { mutableStateOf<Uri?>(null) }
    var selectedAttachmentMime by remember { mutableStateOf<String?>(null) }
    var captureUri by remember { mutableStateOf<Uri?>(null) }
    var videoCaptureUri by remember { mutableStateOf<Uri?>(null) }
    var recordingVoice by remember { mutableStateOf(false) }
    var messageMenu by remember { mutableStateOf<String?>(null) }
    var deleteMessage by remember { mutableStateOf<DirectMessage?>(null) }
    var deleteForEveryone by remember { mutableStateOf(false) }
    var confirmConversationDelete by remember { mutableStateOf(false) }
    var selectedConversations by remember { mutableStateOf(setOf<String>()) }
    var confirmBatchDelete by remember { mutableStateOf(false) }
    var pendingCall by remember { mutableStateOf<Pair<Author, String>?>(null) }
    val galleryLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        selectedAttachment = uri
        selectedAttachmentMime = uri?.let { "image/jpeg" }
    }
    val videoLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        selectedAttachment = uri
        selectedAttachmentMime = uri?.let {
            context.contentResolver.getType(it)?.takeIf { type -> type.startsWith("video/") }
                ?: "video/mp4"
        }
    }
    val cameraLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicture(),
    ) { captured ->
        if (captured) {
            selectedAttachment = captureUri
            selectedAttachmentMime = "image/jpeg"
        }
        captureUri = null
    }
    val videoCaptureLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.CaptureVideo(),
    ) { captured ->
        if (captured) {
            selectedAttachment = videoCaptureUri
            selectedAttachmentMime = "video/mp4"
        }
        videoCaptureUri = null
    }
    val voiceRecorder = remember { VoiceNoteRecorder(context.applicationContext) }
    val voicePermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            runCatching { voiceRecorder.start() }
                .onSuccess { recordingVoice = true }
                .onFailure { vm.errorDialog = it.message ?: "voice recording failed" }
        } else vm.errorDialog = "Microphone permission is required for voice notes"
    }
    DisposableEffect(voiceRecorder) {
        onDispose { voiceRecorder.cancel() }
    }
    val callPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        val request = pendingCall
        if (request != null && grants.values.all { it }) vm.startCall(request.first, request.second)
        else if (request != null) vm.errorDialog = "Microphone and camera permission are required for this call"
        pendingCall = null
    }
    val selected = vm.messageContacts.firstOrNull { it.author.accountId == selectedAccount }
    val conversations = vm.messageContacts.filter { it.direction != null }
    val candidates = vm.messageContacts.filter { contact ->
        contact.direction == null && listOf(
            contact.author.handle,
            contact.author.displayName,
            contact.author.accountId,
        ).any { it.contains(query.trim(), ignoreCase = true) }
    }
    val conversation = vm.directMessages.filter {
        it.peerHandle.equals(selected?.author?.handle, ignoreCase = true)
    }
    LaunchedEffect(selectedAccount, conversation.size) {
        selectedAccount?.let(vm::openConversation)
    }
    androidx.activity.compose.BackHandler {
        when {
            selectedConversations.isNotEmpty() -> selectedConversations = emptySet()
            selectedAccount != null -> selectedAccount = null
            creating -> creating = false
            else -> vm.dmOpen = false
        }
    }
    Box(Modifier.fillMaxSize()) {
        HiveHoneycombBackground()
        Column(Modifier.fillMaxSize()) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(onClick = {
                when {
                    selectedConversations.isNotEmpty() -> selectedConversations = emptySet()
                    selectedAccount != null -> selectedAccount = null
                    creating -> creating = false
                    else -> vm.dmOpen = false
                }
            }) { Text("← Back", color = TextDim) }
            Spacer(Modifier.weight(1f))
            Text(
                when {
                    selectedConversations.isNotEmpty() -> "${selectedConversations.size} SELECTED"
                    selected != null -> selected.author.label.uppercase()
                    creating -> "NEW CHAT"
                    else -> "MESSAGES"
                },
                style = BrandTextStyle, fontSize = 16.sp,
                fontWeight = FontWeight.Black,
                letterSpacing = if (selected == null) 3.sp else 0.sp,
            )
            Spacer(Modifier.weight(1f))
            if (selectedConversations.isNotEmpty()) {
                IconButton(onClick = { selectedConversations = emptySet() }) {
                    Icon(Icons.Outlined.Close, "Cancel selection", tint = TextDim)
                }
                IconButton(onClick = { confirmBatchDelete = true }) {
                    Icon(Icons.Outlined.Delete, "Delete selected conversations", tint = Danger)
                }
            } else if (selected == null && !creating) {
                Box(
                    Modifier.size(48.dp)
                        .semantics {
                            contentDescription = "Contact Nexus-Way Support"
                            role = Role.Button
                        }
                        .clickable {
                        vm.supportOpen = true
                        if (vm.encryptedFoldsReady) vm.refreshSupportThreads()
                    },
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(Icons.Filled.Hexagon, null, tint = Color(0xFF102A56), modifier = Modifier.size(46.dp))
                    Icon(Icons.Outlined.Hexagon, null, tint = Color.White, modifier = Modifier.size(46.dp))
                    Text("!", color = Honey, fontSize = 23.sp, fontWeight = FontWeight.Black)
                }
            } else {
                if (selected?.direction == "accepted") {
                    IconButton(onClick = {
                        pendingCall = selected.author to "voice"
                        callPermissionLauncher.launch(callPermissions("voice"))
                    }) {
                        Icon(Icons.Outlined.Call, "Start secure voice call", tint = Accent2)
                    }
                    IconButton(onClick = {
                        pendingCall = selected.author to "video"
                        callPermissionLauncher.launch(callPermissions("video"))
                    }) {
                        Icon(Icons.Outlined.Videocam, "Start secure video call", tint = Accent2)
                    }
                    IconButton(onClick = { confirmConversationDelete = true }) {
                        Icon(Icons.Outlined.Delete, "Delete conversation", tint = Danger)
                    }
                } else {
                    IconButton(onClick = { vm.refreshMessages() }) {
                        Icon(Icons.Outlined.Refresh, "Refresh", tint = TextDim)
                    }
                }
            }
        }
        HorizontalDivider(color = BorderSoft, thickness = 1.dp)
        if (selected?.direction == "accepted") {
            Text(
                "End-to-end encrypted • Verified device keys",
                color = Accent2,
                fontSize = 12.sp,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
            )
            LazyColumn(
                modifier = Modifier.weight(1f).fillMaxWidth(),
                contentPadding = PaddingValues(horizontal = 12.dp, vertical = 10.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (conversation.isEmpty()) {
                    item {
                        Text(
                            "This chat is ready. Send the first encrypted message.",
                            color = TextDim,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.fillMaxWidth().padding(24.dp),
                        )
                    }
                }
                items(conversation, key = { it.id }) { message ->
                    LaunchedEffect(message.id, message.attachment) {
                        if (message.attachment != null) vm.loadMessageAttachment(message)
                    }
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = if (message.mine) Arrangement.End else Arrangement.Start,
                    ) {
                        Surface(
                            color = if (message.mine) AccentSoft else Panel,
                            shape = HiveCutShape,
                            modifier = Modifier.widthIn(max = 310.dp).hivePanelDepth(message.mine),
                        ) {
                            Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
                                vm.messageImages[message.id]?.let { bytes ->
                                    AsyncImage(
                                        model = bytes,
                                        contentDescription = "Encrypted photo",
                                        contentScale = ContentScale.Crop,
                                        modifier = Modifier
                                            .widthIn(max = 286.dp)
                                            .heightIn(max = 320.dp)
                                            .clip(RoundedCornerShape(6.dp)),
                                    )
                                    if (message.body.isNotBlank()) Spacer(Modifier.height(8.dp))
                                }
                                vm.messageMediaFiles[message.id]?.let { file ->
                                    when {
                                        message.attachment?.mime?.startsWith("audio/") == true ->
                                            EncryptedAudioPlayer(file)
                                        message.attachment?.mime?.startsWith("video/") == true ->
                                            EncryptedVideoPlayer(file)
                                    }
                                    if (message.body.isNotBlank()) Spacer(Modifier.height(8.dp))
                                }
                                Row(verticalAlignment = Alignment.Top) {
                                    if (message.body.isNotBlank()) {
                                        Text(
                                            message.body,
                                            color = TextMain,
                                            fontSize = 15.sp,
                                            modifier = Modifier.weight(1f, fill = false),
                                        )
                                    }
                                    Box {
                                        IconButton(
                                            onClick = { messageMenu = message.id },
                                            modifier = Modifier.size(28.dp),
                                        ) {
                                            Icon(
                                                Icons.Outlined.MoreVert,
                                                contentDescription = "Message options",
                                                tint = TextMuted,
                                                modifier = Modifier.size(17.dp),
                                            )
                                        }
                                        DropdownMenu(
                                            expanded = messageMenu == message.id,
                                            onDismissRequest = { messageMenu = null },
                                        ) {
                                            DropdownMenuItem(
                                                text = { Text("Delete for me") },
                                                onClick = {
                                                    messageMenu = null
                                                    deleteMessage = message
                                                    deleteForEveryone = false
                                                },
                                                leadingIcon = { Icon(Icons.Outlined.Delete, null) },
                                            )
                                            if (message.mine) {
                                                DropdownMenuItem(
                                                    text = { Text("Delete for everyone", color = Danger) },
                                                    onClick = {
                                                        messageMenu = null
                                                        deleteMessage = message
                                                        deleteForEveryone = true
                                                    },
                                                    leadingIcon = {
                                                        Icon(Icons.Outlined.Delete, null, tint = Danger)
                                                    },
                                                )
                                            }
                                        }
                                    }
                                }
                                Row(
                                    modifier = Modifier.align(Alignment.End),
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    Text(timeAgo(message.sent), color = TextDim, fontSize = 11.sp)
                                    if (message.mine) {
                                        Text(" • ${message.status.replaceFirstChar { it.uppercase() }}", color = TextDim, fontSize = 11.sp)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            HorizontalDivider(color = BorderSoft, thickness = 1.dp)
            Column(Modifier.fillMaxWidth().padding(10.dp)) {
                selectedAttachment?.let { uri ->
                    Box(Modifier.padding(bottom = 8.dp)) {
                        if (selectedAttachmentMime?.startsWith("image/") == true) {
                            AsyncImage(
                                model = uri,
                                contentDescription = "Selected photo",
                                contentScale = ContentScale.Crop,
                                modifier = Modifier.size(92.dp).clip(RoundedCornerShape(6.dp)),
                            )
                        } else {
                            Surface(color = PanelHi, shape = RoundedCornerShape(6.dp)) {
                                Row(
                                    Modifier.padding(horizontal = 14.dp, vertical = 12.dp),
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    Icon(
                                        if (selectedAttachmentMime?.startsWith("audio/") == true)
                                            Icons.Outlined.Mic else Icons.Outlined.Movie,
                                        null,
                                        tint = Accent2,
                                    )
                                    Spacer(Modifier.width(8.dp))
                                    Text(
                                        if (selectedAttachmentMime?.startsWith("audio/") == true)
                                            "Voice note ready" else "Video ready",
                                        color = TextMain,
                                    )
                                }
                            }
                        }
                        IconButton(
                            onClick = {
                                selectedAttachment = null
                                selectedAttachmentMime = null
                            },
                            modifier = Modifier.align(Alignment.TopEnd).size(30.dp),
                        ) {
                            Icon(Icons.Outlined.Delete, "Remove photo", tint = Danger)
                        }
                    }
                }
                Row(
                    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    IconButton(
                        onClick = {
                            galleryLauncher.launch(
                                PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                            )
                        },
                        enabled = !vm.busy,
                    ) { Icon(Icons.Outlined.Image, "Choose photo", tint = TextDim) }
                    IconButton(
                        onClick = {
                            newCaptureUri(context).also {
                                captureUri = it
                                cameraLauncher.launch(it)
                            }
                        },
                        enabled = !vm.busy,
                    ) { Icon(Icons.Outlined.PhotoCamera, "Take photo", tint = TextDim) }
                    IconButton(
                        onClick = {
                            videoLauncher.launch(
                                PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.VideoOnly),
                            )
                        },
                        enabled = !vm.busy && !recordingVoice,
                    ) { Icon(Icons.Outlined.Movie, "Choose video", tint = TextDim) }
                    IconButton(
                        onClick = {
                            newVideoCaptureUri(context).also {
                                videoCaptureUri = it
                                videoCaptureLauncher.launch(it)
                            }
                        },
                        enabled = !vm.busy && !recordingVoice,
                    ) { Icon(Icons.Outlined.Videocam, "Record video", tint = TextDim) }
                    IconButton(
                        onClick = {
                            if (recordingVoice) {
                                voiceRecorder.stop()?.let { file ->
                                    selectedAttachment = Uri.fromFile(file)
                                    selectedAttachmentMime = "audio/mp4"
                                }
                                recordingVoice = false
                            } else {
                                voicePermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
                            }
                        },
                        enabled = !vm.busy,
                    ) {
                        Icon(
                            if (recordingVoice) Icons.Outlined.Pause else Icons.Outlined.Mic,
                            if (recordingVoice) "Stop voice note" else "Record voice note",
                            tint = if (recordingVoice) Danger else TextDim,
                        )
                    }
                    if (recordingVoice) {
                        Text("Recording…", color = Danger, fontSize = 12.sp)
                    }
                }
                Row(
                    Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = draft,
                        onValueChange = { draft = it },
                        placeholder = { Text("Encrypted message") },
                        enabled = !vm.busy,
                        maxLines = 4,
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(Modifier.width(6.dp))
                    IconButton(
                        enabled = (draft.isNotBlank() || selectedAttachment != null) &&
                            !vm.busy && !recordingVoice,
                        onClick = {
                            vm.sendDirect(
                                selected.author.accountId,
                                draft,
                                selectedAttachment,
                                selectedAttachmentMime,
                            )
                            draft = ""
                            selectedAttachment = null
                            selectedAttachmentMime = null
                        },
                    ) {
                        if (vm.busy) {
                            CircularProgressIndicator(
                                color = Accent2,
                                strokeWidth = 2.dp,
                                modifier = Modifier.size(22.dp),
                            )
                        } else {
                            Icon(
                                Icons.AutoMirrored.Outlined.Send,
                                contentDescription = "Send encrypted message",
                                tint = Accent2,
                            )
                        }
                    }
                }
            }
        } else if (creating) {
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                leadingIcon = { Icon(Icons.Outlined.Search, null, tint = TextDim) },
                placeholder = { Text("Search followers and following") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
            )
            LazyColumn(Modifier.weight(1f).fillMaxWidth()) {
                if (candidates.isEmpty()) {
                    item {
                        Text(
                            if (query.isBlank()) "No available contacts to request right now."
                            else "No matching contacts.",
                            color = TextDim,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.fillMaxWidth().padding(32.dp),
                        )
                    }
                }
                items(candidates, key = { it.author.accountId }) { contact ->
                    val stateLabel = if (vm.following.any { it.accountId == contact.author.accountId }) {
                        "Following"
                    } else {
                        "Follows you"
                    }
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .padding(horizontal = 16.dp, vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        AuthorAvatar(vm, contact.author, 42.dp)
                        Spacer(Modifier.width(12.dp))
                        Column(Modifier.weight(1f)) {
                            Text(contact.author.label, color = TextMain, fontWeight = FontWeight.SemiBold)
                            Text("@${contact.author.handle} • $stateLabel", color = TextDim, fontSize = 12.sp)
                        }
                        TextButton(
                            onClick = {
                                vm.requestChat(contact.author)
                                creating = false
                                query = ""
                            },
                            enabled = !vm.busy,
                        ) { Text("Request", color = Accent2) }
                    }
                    HorizontalDivider(color = BorderSoft, thickness = 1.dp)
                }
            }
        } else {
            if (selectedConversations.isEmpty()) {
                Button(
                    onClick = { creating = true },
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 12.dp)
                        .height(46.dp),
                ) {
                    Icon(Icons.Outlined.Add, null)
                    Spacer(Modifier.width(8.dp))
                    Text("New conversation")
                }
            }
            LazyColumn(
                Modifier.weight(1f).fillMaxWidth(),
                contentPadding = PaddingValues(horizontal = 12.dp, vertical = 4.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                if (conversations.isEmpty()) {
                    item {
                        Column(
                            Modifier.fillMaxWidth().padding(32.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                        ) {
                            Icon(
                                Icons.AutoMirrored.Outlined.Send,
                                contentDescription = null,
                                tint = TextMuted,
                                modifier = Modifier.size(42.dp),
                            )
                            Spacer(Modifier.height(14.dp))
                            Text("No conversations yet", color = TextMain, fontWeight = FontWeight.SemiBold)
                            Spacer(Modifier.height(6.dp))
                            Text(
                                "Use New conversation to request an encrypted chat.",
                                color = TextDim,
                                textAlign = TextAlign.Center,
                            )
                        }
                    }
                }
                items(conversations, key = { it.author.accountId }) { contact ->
                    val isSelected = contact.author.accountId in selectedConversations
                    val stateLabel = when (contact.direction) {
                        "incoming" -> "Message request"
                        "outgoing" -> "Waiting for acceptance"
                        else -> "Encrypted chat"
                    }
                    val last = vm.directMessages
                        .filter { it.peerHandle.equals(contact.author.handle, ignoreCase = true) }
                        .maxByOrNull { it.sent }
                    Surface(
                        color = if (isSelected) AccentSoft else Panel,
                        shape = HiveCutShape,
                        border = BorderStroke(
                            1.dp,
                            when {
                                isSelected -> Accent2
                                contact.direction == "incoming" -> Accent2.copy(alpha = 0.45f)
                                else -> BorderSoft
                            },
                        ),
                        modifier = Modifier
                            .fillMaxWidth()
                            .hivePanelDepth(isSelected)
                            .combinedClickable(
                                onClick = {
                                    if (selectedConversations.isNotEmpty()) {
                                        selectedConversations = if (isSelected) {
                                            selectedConversations - contact.author.accountId
                                        } else {
                                            selectedConversations + contact.author.accountId
                                        }
                                    } else if (contact.direction == "accepted") {
                                        selectedAccount = contact.author.accountId
                                        vm.openConversation(contact.author.accountId)
                                    }
                                },
                                onLongClick = {
                                    selectedConversations = selectedConversations + contact.author.accountId
                                },
                            ),
                    ) {
                        Column(Modifier.padding(horizontal = 14.dp, vertical = 13.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                AuthorAvatar(vm, contact.author, 50.dp)
                                Spacer(Modifier.width(13.dp))
                                Column(Modifier.weight(1f)) {
                                    Text(
                                        contact.author.label,
                                        color = TextMain,
                                        fontWeight = FontWeight.SemiBold,
                                        fontSize = 15.sp,
                                    )
                                    Text(
                                        last?.body?.ifBlank {
                                            if (last.attachment != null) "Media message" else stateLabel
                                        } ?: stateLabel,
                                        color = if (contact.direction == "incoming") Accent2 else TextDim,
                                        fontSize = 13.sp,
                                        maxLines = 1,
                                    )
                                }
                                if (selectedConversations.isNotEmpty()) {
                                    Checkbox(
                                        checked = isSelected,
                                        onCheckedChange = { checked ->
                                            selectedConversations = if (checked) {
                                                selectedConversations + contact.author.accountId
                                            } else {
                                                selectedConversations - contact.author.accountId
                                            }
                                        },
                                    )
                                } else if (contact.direction == "accepted") {
                                    Icon(Icons.Outlined.ChevronRight, "Open conversation", tint = TextDim)
                                } else if (contact.direction == "outgoing") {
                                    Text("Pending", color = TextMuted, fontSize = 12.sp)
                                }
                            }
                            if (contact.direction == "incoming") {
                                Row(
                                    Modifier.fillMaxWidth().padding(top = 10.dp),
                                    horizontalArrangement = Arrangement.End,
                                ) {
                                    TextButton(onClick = { vm.respondToChat(contact.author, false) }) {
                                        Text("Decline", color = TextDim)
                                    }
                                    Spacer(Modifier.width(6.dp))
                                    Button(
                                        onClick = { vm.respondToChat(contact.author, true) },
                                        enabled = !vm.busy,
                                    ) { Text("Accept") }
                                }
                            }
                        }
                    }
                }
            }
        }
        if (vm.status.isNotEmpty()) {
            Surface(color = Panel, shape = HiveCutShape, modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp).hivePanelDepth()) {
                Text(
                    vm.status,
                    color = TextDim,
                    fontSize = 12.sp,
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp),
                )
            }
        }
        }
        deleteMessage?.let { message ->
            AlertDialog(
                onDismissRequest = { deleteMessage = null },
                icon = { Icon(Icons.Outlined.Delete, null, tint = Danger) },
                title = {
                    Text(if (deleteForEveryone) "Delete for everyone?" else "Delete message?")
                },
                text = {
                    Text(
                        if (deleteForEveryone) {
                            "Nexus will retract queued copies and ask every Nexus device to remove this message. Screenshots, notification previews, exports, and copies made by modified clients cannot be recalled."
                        } else {
                            "This removes the message only from this account's local history on this device."
                        },
                    )
                },
                confirmButton = {
                    TextButton(onClick = {
                        vm.deleteMessage(message, deleteForEveryone)
                        deleteMessage = null
                    }) { Text("Delete", color = Danger) }
                },
                dismissButton = {
                    TextButton(onClick = { deleteMessage = null }) { Text("Cancel") }
                },
            )
        }
        if (confirmConversationDelete && selected != null) {
            AlertDialog(
                onDismissRequest = { confirmConversationDelete = false },
                icon = { Icon(Icons.Outlined.Delete, null, tint = Danger) },
                title = { Text("Delete conversation?") },
                text = {
                    Text("This removes the conversation and its messages from this device. It does not delete the other person's copies.")
                },
                confirmButton = {
                    TextButton(onClick = {
                        vm.deleteConversation(selected.author.accountId, selected.author.handle)
                        selectedAccount = null
                        confirmConversationDelete = false
                    }) { Text("Delete", color = Danger) }
                },
                dismissButton = {
                    TextButton(onClick = { confirmConversationDelete = false }) { Text("Cancel") }
                },
            )
        }
        if (confirmBatchDelete && selectedConversations.isNotEmpty()) {
            AlertDialog(
                onDismissRequest = { confirmBatchDelete = false },
                icon = { Icon(Icons.Outlined.Delete, null, tint = Danger) },
                title = { Text("Delete ${selectedConversations.size} conversations?") },
                text = {
                    Text(
                        "This removes the selected conversations and their messages from your synchronized account history. It does not delete the other people's copies.",
                    )
                },
                confirmButton = {
                    TextButton(onClick = {
                        vm.deleteConversations(
                            conversations
                                .filter { it.author.accountId in selectedConversations }
                                .associate { it.author.accountId to it.author.handle },
                        )
                        selectedConversations = emptySet()
                        confirmBatchDelete = false
                    }) { Text("Delete", color = Danger) }
                },
                dismissButton = {
                    TextButton(onClick = { confirmBatchDelete = false }) { Text("Cancel") }
                },
            )
        }
    }
}

@Composable
private fun EncryptedAudioPlayer(file: File) {
    val player = remember(file) {
        MediaPlayer().apply {
            setDataSource(file.absolutePath)
            prepare()
        }
    }
    var playing by remember(file) { mutableStateOf(false) }
    var position by remember(file) { mutableFloatStateOf(0f) }
    val duration = player.duration.coerceAtLeast(1)
    DisposableEffect(player) {
        player.setOnCompletionListener {
            playing = false
            position = 0f
            player.seekTo(0)
        }
        onDispose { player.release() }
    }
    LaunchedEffect(playing, player) {
        while (playing) {
            position = player.currentPosition.toFloat()
            kotlinx.coroutines.delay(250)
        }
    }
    Row(
        Modifier.widthIn(min = 220.dp, max = 286.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        IconButton(onClick = {
            if (player.isPlaying) player.pause() else player.start()
            playing = player.isPlaying
        }) {
            Icon(
                if (playing) Icons.Outlined.Pause else Icons.Outlined.PlayArrow,
                if (playing) "Pause voice note" else "Play voice note",
                tint = Accent2,
            )
        }
        Slider(
            value = position.coerceIn(0f, duration.toFloat()),
            onValueChange = {
                position = it
                player.seekTo(it.toInt())
            },
            valueRange = 0f..duration.toFloat(),
            modifier = Modifier.weight(1f),
        )
        Text("${duration / 1_000}s", color = TextDim, fontSize = 11.sp)
    }
}

@Composable
private fun EncryptedVideoPlayer(file: File) {
    AndroidView(
        factory = { context ->
            VideoView(context).apply {
                setVideoPath(file.absolutePath)
                setMediaController(MediaController(context).also { it.setAnchorView(this) })
            }
        },
        modifier = Modifier
            .widthIn(max = 286.dp)
            .fillMaxWidth()
            .aspectRatio(16f / 9f)
            .clip(RoundedCornerShape(6.dp)),
    )
}

// ------------------------------------------------------------- post detail

/** One post as a full-screen overlay (opened from an alert or permalink). */
@Composable
fun PostDetailPane(vm: ConnectViewModel) {
    val p = vm.detailPost ?: return
    androidx.activity.compose.BackHandler { vm.closePostDetail() }
    Surface(Modifier.fillMaxSize(), color = Color.Transparent) {
        Column(Modifier.fillMaxSize()) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(onClick = { vm.closePostDetail() }) { Text("← Back", color = TextDim) }
                Spacer(Modifier.weight(1f))
                Text(
                    "POST",
                    style = BrandTextStyle, fontSize = 16.sp,
                    fontWeight = FontWeight.Black, letterSpacing = 3.sp,
                )
                Spacer(Modifier.weight(1f))
                Spacer(Modifier.width(64.dp)) // balance the back button
            }
            HorizontalDivider(color = BorderSoft, thickness = 1.dp)
            LazyColumn(Modifier.fillMaxSize()) {
                item { Spacer(Modifier.height(8.dp)) }
                item { PostCard(vm, p) }
                item { Spacer(Modifier.height(60.dp)) }
            }
        }
    }
}

// ---------------------------------------------------------------- discover

@Composable
fun DiscoverPane(vm: ConnectViewModel) {
    var query by remember { mutableStateOf("") }
    Column(Modifier.fillMaxSize()) {
        OutlinedTextField(
            value = query,
            onValueChange = {
                query = it
                vm.search(it)
            },
            placeholder = { Text("Search people by handle or name") },
            singleLine = true,
            leadingIcon = {
                Icon(Icons.Outlined.Search, null, tint = TextDim, modifier = Modifier.size(20.dp))
            },
            modifier = Modifier.fillMaxWidth().padding(16.dp),
        )
        when {
            vm.searching -> Box(Modifier.fillMaxWidth().padding(24.dp), Alignment.Center) {
                CircularProgressIndicator(color = Accent2, modifier = Modifier.size(24.dp))
            }
            query.isBlank() -> Column(
                Modifier.fillMaxWidth().padding(32.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    "Find the people you actually know.",
                    color = TextDim, textAlign = TextAlign.Center,
                )
                Spacer(Modifier.height(4.dp))
                Text(
                    "No suggestions, no \"people you may know\" — your graph is yours.",
                    color = TextDim, fontSize = 13.sp, textAlign = TextAlign.Center,
                )
            }
            vm.searchResults.isEmpty() -> Text(
                "Nobody found for \"$query\"",
                color = TextDim,
                modifier = Modifier.padding(24.dp).fillMaxWidth(),
                textAlign = TextAlign.Center,
            )
            else -> LazyColumn {
                items(vm.searchResults, key = { it.author.accountId }) { hit ->
                    AuthorRow(vm, hit.author, onOpen = { vm.openProfile(hit.author.accountId) }) {
                        FollowButton(vm, hit.author, hit.followState)
                    }
                }
            }
        }
    }
}

@Composable
fun FollowButton(vm: ConnectViewModel, author: Author, followState: String?) {
    when (followState) {
        "accepted" -> OutlinedButton(onClick = { vm.unfollow(author) }) { Text("Following") }
        "requested" -> OutlinedButton(onClick = { vm.unfollow(author) }) { Text("Requested") }
        else -> Button(onClick = { vm.requestFollow(author.accountId) }) { Text("Follow") }
    }
}

// ------------------------------------------------------------------ alerts

@Composable
fun AlertsPane(vm: ConnectViewModel) {
    var dismissedAlerts by remember { mutableStateOf(setOf<String>()) }
    val visibleAlerts = vm.alerts.filter { it.id !in dismissedAlerts }
    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(bottom = 72.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item {
            Text(
                "Alerts",
                color = TextMain, fontSize = 20.sp, fontWeight = FontWeight.Bold,
                modifier = Modifier.padding(16.dp),
            )
        }
        val reqRows = vm.requestRows
        if (reqRows.isNotEmpty()) {
            item {
                Text(
                    "FOLLOW REQUESTS",
                    color = Accent2, fontSize = 12.sp, fontWeight = FontWeight.Bold,
                    letterSpacing = 1.sp,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
            items(reqRows, key = { "req-" + it.accountId }) { a ->
                FollowRequestRow(vm, a, onOpen = { vm.openProfile(a.accountId) })
            }
            item { HorizontalDivider(color = PanelHi) }
        }
        if (visibleAlerts.isEmpty()) {
            item {
                Text(
                    "Nothing yet. Alerts appear when people follow you or " +
                        "react to your posts.",
                    color = TextDim,
                    modifier = Modifier.padding(24.dp).fillMaxWidth(),
                    textAlign = TextAlign.Center,
                )
            }
        }
        items(visibleAlerts, key = { it.id }) { a ->
            val what = when (a.kind) {
                "follow_request" -> "wants to follow you"
                "follow_accepted" -> "accepted your follow request"
                "follow" -> "started following you"
                "post" -> "shared a new post"
                "comment" -> "commented on your post"
                "reaction" -> "reacted to your post"
                "mention" -> "mentioned you"
                "support_reply" -> "replied to your support request"
                "fold_invite" -> "invited you to a Fold"
                "fold_joined" -> "joined your Fold"
                "fold_post" -> "posted in a Fold"
                "message" -> "sent you an encrypted message"
                "message_request" -> "sent you a message request"
                "system" -> a.title.ifEmpty { "Announcement" }
                else -> a.kind
            }
            Surface(
                color = if (a.seen) Panel.copy(alpha = 0.78f) else PanelHi.copy(alpha = 0.90f),
                shape = HiveCutShape,
                border = BorderStroke(1.dp, if (a.seen) BorderSoft else Accent2.copy(alpha = 0.35f)),
                modifier = Modifier
                    .padding(horizontal = 12.dp)
                    .fillMaxWidth()
                    .hivePanelDepth(!a.seen),
            ) {
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clickable {
                            when (a.kind) {
                                "support_reply" -> {
                                    vm.supportOpen = true
                                    vm.refreshSupportThreads()
                                }
                                "message", "message_request" -> vm.dmOpen = true
                                "system" -> { /* announcement: content is inline */ }
                                "fold_invite", "fold_joined", "fold_post" ->
                                    vm.openFoldActivity(a.kind, a.subjectId, a.foldId)
                                "comment", "reaction" -> if (a.foldId.isNotEmpty()) {
                                    vm.openFoldActivity(a.kind, a.subjectId, a.foldId)
                                } else if (a.subjectId.isNotEmpty()) {
                                    vm.openPostDetail(a.subjectId, withComments = a.kind == "comment")
                                } else vm.openProfile(a.from.accountId)
                                "comment", "mention" ->
                                    if (a.subjectId.isNotEmpty()) {
                                        vm.openPostDetail(a.subjectId, withComments = true)
                                    } else vm.openProfile(a.from.accountId)
                                "post", "reaction" ->
                                    if (a.subjectId.isNotEmpty()) {
                                        vm.openPostDetail(a.subjectId)
                                    } else vm.openProfile(a.from.accountId)
                                else -> vm.openProfile(a.from.accountId)
                            }
                        }
                        .padding(horizontal = 12.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (a.kind == "system") {
                        Box(
                            Modifier.size(40.dp).clip(CircleShape)
                                .background(Accent2.copy(alpha = 0.18f)),
                            contentAlignment = Alignment.Center,
                        ) {
                            Icon(
                                Icons.Outlined.Campaign,
                                "Announcement",
                                tint = Accent2,
                                modifier = Modifier.size(24.dp),
                            )
                        }
                    } else {
                        AuthorAvatar(vm, a.from, 40.dp)
                    }
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f)) {
                        if (a.kind == "system") {
                            Text(
                                a.title.ifEmpty { "Announcement" },
                                color = TextMain, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
                            )
                            if (a.body.isNotEmpty()) {
                                Text(
                                    a.body,
                                    color = TextDim, fontSize = 13.sp,
                                    modifier = Modifier.padding(top = 2.dp),
                                )
                            }
                        } else {
                            Text(
                                buildString { append(a.from.label); append(" "); append(what) },
                                color = TextMain, fontSize = 14.sp,
                            )
                        }
                        Text(timeAgo(a.created), color = TextDim, fontSize = 12.sp)
                    }
                    if (!a.seen) {
                        Box(Modifier.size(8.dp).clip(CircleShape).background(Accent2))
                        Spacer(Modifier.width(8.dp))
                    }
                    IconButton(onClick = { dismissedAlerts = dismissedAlerts + a.id; vm.deleteAlert(a.id) }) {
                        Text("×", color = TextDim, fontSize = 20.sp)
                    }
                }
            }
        }
    }
}

// ----------------------------------------------------------------- profile

@Composable
fun ProfileScreen(
    vm: ConnectViewModel,
    p: Profile,
    posts: List<Post>,
    own: Boolean,
    onBack: (() -> Unit)?,
) {
    var editing by remember { mutableStateOf(false) }
    var listShown by remember { mutableStateOf<String?>(null) } // following|followers
    if (onBack != null) {
        androidx.activity.compose.BackHandler { onBack() }
    }

    Surface(Modifier.fillMaxSize(), color = Color.Transparent) {
        LazyColumn(Modifier.fillMaxSize()) {
            item {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (onBack != null) {
                        TextButton(onClick = onBack) { Text("← Back", color = TextDim) }
                    }
                    Spacer(Modifier.weight(1f))
                    if (own) {
                        IconButton(onClick = { vm.openSettings() }) {
                            Icon(Icons.Outlined.Settings, "Settings", tint = TextDim)
                        }
                    }
                }
            }
            item {
                Column(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Avatar(p.avatarBlob, p.handle, 88.dp, vm)
                    Spacer(Modifier.height(10.dp))
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            p.displayName.ifEmpty { "@${p.handle}" },
                            color = TextMain, fontSize = 20.sp, fontWeight = FontWeight.Bold,
                        )
                        Spacer(Modifier.width(6.dp))
                        RoleHexBadge(p.founder, p.communityRole, p.membershipTier)
                    }
                    if (p.displayName.isNotEmpty()) {
                        Text("@${p.handle}", color = TextDim, fontSize = 14.sp)
                    }
                    if (p.bio.isNotEmpty()) {
                        Spacer(Modifier.height(8.dp))
                        Text(
                            p.bio, color = TextMain,
                            fontSize = 14.sp, textAlign = TextAlign.Center,
                        )
                    }
                    Spacer(Modifier.height(14.dp))
                    Row {
                        StatCell("${p.postCount}", "posts") {}
                        StatCell("${p.followers}", "followers") {
                            if (own) listShown = "followers"
                        }
                        StatCell("${p.following}", "following") {
                            if (own) listShown = "following"
                        }
                    }
                    Spacer(Modifier.height(14.dp))
                    if (own) {
                        Button(
                            onClick = { editing = true },
                            modifier = Modifier.fillMaxWidth(),
                        ) { Text("Edit profile") }
                    } else {
                        Row(Modifier.fillMaxWidth()) {
                            Box(Modifier.weight(1f)) {
                                when (p.followState) {
                                    "accepted" -> OutlinedButton(
                                        onClick = {
                                            vm.unfollow(
                                                Author(p.accountId, p.handle, p.displayName),
                                            )
                                        },
                                        modifier = Modifier.fillMaxWidth(),
                                    ) { Text("Following") }
                                    "requested" -> OutlinedButton(
                                        onClick = {
                                            vm.unfollow(
                                                Author(p.accountId, p.handle, p.displayName),
                                            )
                                        },
                                        modifier = Modifier.fillMaxWidth(),
                                    ) { Text("Requested") }
                                    else -> Button(
                                        onClick = { vm.requestFollow(p.accountId) },
                                        modifier = Modifier.fillMaxWidth(),
                                    ) { Text("Follow") }
                                }
                            }
                            Spacer(Modifier.width(8.dp))
                            OutlinedButton(onClick = {
                                vm.block(Author(p.accountId, p.handle, p.displayName))
                            }) { Text("Block") }
                        }
                    }
                    Spacer(Modifier.height(10.dp))
                }
            }
            if (posts.isEmpty()) {
                item {
                    Text(
                        if (own) "No posts yet — tap + to share something."
                        else "No posts you can see. Follow to see more.",
                        color = TextDim,
                        modifier = Modifier.padding(24.dp).fillMaxWidth(),
                        textAlign = TextAlign.Center,
                    )
                }
            }
            items(posts, key = { it.postId }) { post -> PostCard(vm, post) }
            item { Spacer(Modifier.height(60.dp)) }
        }
    }

    if (editing) {
        EditProfileSheet(vm, p, onClose = { editing = false })
    }

    if (own && vm.settingsOpen) {
        SettingsSheet(vm, onClose = { vm.settingsOpen = false })
    }

    listShown?.let { which ->
        FollowListSheet(vm, which, onClose = { listShown = null })
    }
}

// ---------------------------------------------------------------- settings

private enum class SettingsSection(val title: String) {
    Home("Settings"),
    Account("Account & appearance"),
    Privacy("Privacy & safety"),
    Messaging("Messaging"),
    Invites("Invitations"),
    Security("Security & devices"),
    Data("Data & account"),
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun SettingsSheet(vm: ConnectViewModel, onClose: () -> Unit) {
    var section by remember { mutableStateOf(SettingsSection.Home) }
    var newHandle by remember { mutableStateOf(vm.handle) }
    var newPassword by remember { mutableStateOf("") }
    var confirmDelete by remember { mutableStateOf(false) }
    var deleteHandle by remember { mutableStateOf("") }
    var deletePassword by remember { mutableStateOf("") }
    LaunchedEffect(Unit) { vm.refreshAccountSettings(); vm.refreshFolds() }
    val canIssueInvites = vm.myProfile?.let {
        it.founder || it.communityRole == "steward"
    } == true
    LaunchedEffect(canIssueInvites) {
        if (canIssueInvites) vm.refreshInvites()
    }
    BackHandler {
        if (section == SettingsSection.Home) onClose() else section = SettingsSection.Home
    }
    Dialog(
        onDismissRequest = {
            if (section == SettingsSection.Home) onClose() else section = SettingsSection.Home
        },
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Surface(Modifier.fillMaxSize(), color = Panel) {
            LazyColumn(
                Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)
                    .padding(bottom = 16.dp),
            ) {
            item {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 2.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    IconButton(
                        onClick = {
                            if (section == SettingsSection.Home) onClose()
                            else section = SettingsSection.Home
                        },
                    ) {
                        Icon(
                            if (section == SettingsSection.Home) Icons.Outlined.Close
                            else Icons.AutoMirrored.Outlined.ArrowBack,
                            if (section == SettingsSection.Home) "Close settings" else "Back to settings",
                            tint = TextDim,
                        )
                    }
                    Text(
                        section.title,
                        color = TextMain,
                        fontWeight = FontWeight.Bold,
                        fontSize = 18.sp,
                    )
                }
            }

            if (section == SettingsSection.Home) {
                item {
                    val profile = vm.myProfile
                    Row(
                        Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Avatar(profile?.avatarBlob, profile?.handle ?: vm.handle, 54.dp, vm)
                        Spacer(Modifier.width(12.dp))
                        Column(Modifier.weight(1f)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    profile?.displayName?.ifEmpty { "@${profile.handle}" }
                                        ?: "@${vm.handle}",
                                    color = TextMain,
                                    fontWeight = FontWeight.Bold,
                                    fontSize = 17.sp,
                                )
                                profile?.let {
                                    Spacer(Modifier.width(6.dp))
                                    RoleHexBadge(it.founder, it.communityRole, it.membershipTier)
                                }
                            }
                            Text("@${profile?.handle ?: vm.handle}", color = TextMuted, fontSize = 13.sp)
                        }
                    }
                    HorizontalDivider(color = BorderSoft)
                }
                item {
                    SettingsNavRow(
                        Icons.Outlined.Person,
                        "Account & appearance",
                        "Handle and theme",
                    ) { section = SettingsSection.Account }
                    SettingsNavRow(
                        Icons.Outlined.Security,
                        "Privacy & safety",
                        "Discovery, followers, comments, and blocks",
                    ) { section = SettingsSection.Privacy }
                    SettingsNavRow(
                        Icons.Outlined.MailOutline,
                        "Messaging",
                        if (vm.autoAcceptMessageInvites) "Message invites auto-accept" else "Message invites require approval",
                    ) { section = SettingsSection.Messaging }
                    SettingsNavRow(
                        Icons.Outlined.Group,
                        "Folds",
                        "${vm.folds.count { it.owned }} owned · ${vm.folds.count { !it.owned }} joined",
                    ) {
                        vm.settingsOpen = false
                        vm.foldsOpen = true
                    }
                    if (canIssueInvites) {
                        SettingsNavRow(
                            Icons.Outlined.VpnKey,
                            "Invitations",
                            "${vm.invites.count { it.usedHandle == null }} unused · ${vm.invites.count { it.usedHandle != null }} redeemed",
                        ) { section = SettingsSection.Invites }
                    }
                    SettingsNavRow(
                        Icons.Outlined.Security,
                        "Security & devices",
                        "Recovery password · ${vm.deviceRows.count { !it.revoked }} active devices",
                    ) { section = SettingsSection.Security }
                    SettingsNavRow(
                        Icons.Outlined.Storage,
                        "Data & account",
                        "Export, sign out, or delete account",
                    ) { section = SettingsSection.Data }
                }
            }

            if (section == SettingsSection.Account) item {
                SettingsLabel("APPEARANCE")
                Surface(
                    color = Panel.copy(alpha = 0.86f),
                    shape = HiveCutShape,
                    border = BorderStroke(1.dp, BorderSoft),
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 6.dp).fillMaxWidth().hivePanelDepth(),
                ) {
                    Column(Modifier.padding(14.dp)) {
                        Text("Theme", color = TextMain, fontWeight = FontWeight.SemiBold)
                        Text(
                            "Choose the HIVE dark theme or the white-and-gold light theme.",
                            color = TextMuted, fontSize = 12.sp, modifier = Modifier.padding(top = 4.dp),
                        )
                        Row(Modifier.padding(top = 10.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(
                                onClick = { vm.changeThemeMode("dark") },
                                enabled = vm.themeMode != "dark",
                            ) { Text("Dark") }
                            OutlinedButton(
                                onClick = { vm.changeThemeMode("light") },
                                enabled = vm.themeMode != "light",
                            ) { Text("Light") }
                        }
                    }
                }
            }

            if (section == SettingsSection.Account) item {
                SettingsLabel("UPDATES")
                SettingsSwitch(
                    "Automatic updates",
                    "Verified releases from your HIVE install automatically. " +
                        "The first update asks once; after that they apply silently.",
                    vm.autoUpdateEnabled,
                ) { vm.changeAutoUpdate(it) }
            }

            // ---- handle
            if (section == SettingsSection.Account) item {
                SettingsLabel("HANDLE")
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = newHandle,
                        onValueChange = { newHandle = it },
                        singleLine = true,
                        prefix = { Text("@", color = TextDim) },
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(Modifier.width(8.dp))
                    Button(
                        onClick = { vm.changeHandle(newHandle) },
                        enabled = newHandle.trim().removePrefix("@")
                            .let { it.isNotEmpty() && it != vm.handle } && !vm.busy,
                    ) { Text("Change") }
                }
                Text(
                    "Your handle is how people find you. Links to your old handle stop working.",
                    color = TextMuted, fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                )
            }

            // ---- privacy
            if (section == SettingsSection.Privacy) item {
                SettingsLabel("PRIVACY")
                SettingsSwitch(
                    "Appear in search",
                    "Off = nobody can find you in Discover. Existing followers keep seeing you.",
                    vm.setDiscoverable,
                ) { vm.saveSettings(it, vm.setAutoAccept, vm.setCommentsFrom) }
                SettingsSwitch(
                    "Auto-accept followers",
                    "On = anyone can follow you instantly. Off = you approve every request.",
                    vm.setAutoAccept,
                ) { vm.saveSettings(vm.setDiscoverable, it, vm.setCommentsFrom) }

                Text(
                    "Who can comment on your posts",
                    color = TextMain, fontSize = 14.sp,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 6.dp),
                )
                FlowRow(
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    CommentPolicyChip("Anyone who can see", "viewers", vm)
                    CommentPolicyChip("Followers", "followers", vm)
                    CommentPolicyChip("Nobody", "off", vm)
                }
            }

            // ---- legal documents (review any time; acceptance is recorded at
            // sign-up and re-requested in-app when a document version changes)
            if (section == SettingsSection.Privacy) item {
                var viewingDoc by remember { mutableStateOf<String?>(null) }
                SettingsLabel("LEGAL")
                val terms = vm.legalDocs.firstOrNull { it.doc == "terms" }
                val privacy = vm.legalDocs.firstOrNull { it.doc == "privacy" }
                SettingsNavRow(
                    Icons.Outlined.Description,
                    "Terms of Service",
                    terms?.let { "Version ${it.version}" } ?: "Review the current terms",
                ) { viewingDoc = "terms" }
                SettingsNavRow(
                    Icons.Outlined.Description,
                    "Privacy Policy",
                    privacy?.let { "Version ${it.version}" } ?: "Review the current policy",
                ) { viewingDoc = "privacy" }
                viewingDoc?.let { doc ->
                    LegalDocViewerDialog(vm, doc, onClose = { viewingDoc = null })
                }
            }

            if (section == SettingsSection.Messaging) item {
                SettingsLabel("MESSAGING")
                SettingsSwitch(
                    "Auto-accept message invites",
                    "On = encrypted conversation invites are accepted automatically. Off = you approve each invite.",
                    vm.autoAcceptMessageInvites,
                ) { vm.changeAutoAcceptMessageInvites(it) }
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text("Message history sync", color = TextMain, fontSize = 14.sp)
                        Text(
                            when {
                                vm.messageHistorySyncing -> "Syncing encrypted history…"
                                vm.messageHistoryLastSync > 0 ->
                                    "Automatic · ${vm.messageHistoryDeviceCount} devices · last synced " +
                                        timeAgo(vm.messageHistoryLastSync)
                                else -> "Automatic on launch and after message changes"
                            },
                            color = TextMuted,
                            fontSize = 12.sp,
                        )
                    }
                    OutlinedButton(
                        onClick = { vm.syncMessageHistory() },
                        enabled = !vm.messageHistorySyncing && !vm.busy,
                    ) { Text("Sync now") }
                }
            }

            // ---- follow requests
            if (section == SettingsSection.Privacy) {
                item { SettingsLabel("FOLLOW REQUESTS") }
                val reqRows = vm.requestRows
                if (reqRows.isEmpty()) {
                    item {
                        Text(
                            "No pending requests.",
                            color = TextMuted, fontSize = 13.sp,
                            modifier = Modifier.padding(horizontal = 20.dp),
                        )
                    }
                }
                items(reqRows, key = { "set-req-" + it.accountId }) { a ->
                    FollowRequestRow(vm, a, onOpen = { onClose(); vm.openProfile(a.accountId) })
                }
            }

            // ---- blocked users
            if (section == SettingsSection.Privacy) {
                item { SettingsLabel("BLOCKED USERS") }
                if (vm.blockedUsers.isEmpty()) {
                    item {
                        Text(
                            "Nobody blocked.",
                            color = TextMuted, fontSize = 13.sp,
                            modifier = Modifier.padding(horizontal = 20.dp),
                        )
                    }
                }
                items(vm.blockedUsers, key = { "blk-" + it.accountId }) { a ->
                    Row(
                        Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(
                                a.displayName.ifEmpty { "@" + a.handle },
                                color = TextMain, fontSize = 14.sp,
                            )
                            Text("@" + a.handle, color = TextMuted, fontSize = 12.sp)
                        }
                        OutlinedButton(onClick = { vm.unblockUser(a) }) { Text("Unblock") }
                    }
                }
            }

            // ---- devices
            if (section == SettingsSection.Security) item { SettingsLabel("DEVICES") }
            if (section == SettingsSection.Security) items(vm.deviceRows, key = { "dev-" + it.id }) { d ->
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(
                            d.name + if (d.revoked) "  (revoked)" else "",
                            color = if (d.revoked) TextMuted else TextMain, fontSize = 14.sp,
                        )
                        if (d.lastSeen > 0) Text(
                            "last seen " + android.text.format.DateUtils.getRelativeTimeSpanString(d.lastSeen * 1000),
                            color = TextMuted, fontSize = 12.sp,
                        )
                    }
                    if (!d.revoked) OutlinedButton(onClick = { vm.revokeDevice(d) }) {
                        Text("Revoke", color = Danger)
                    }
                }
            }
            if (section == SettingsSection.Security) item {
                Text(
                    "Revoking a device signs it out immediately and it can no longer access the account.",
                    color = TextMuted, fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                )
            }

            // ---- security
            if (section == SettingsSection.Security) item {
                SettingsLabel("SECURITY")
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = newPassword,
                        onValueChange = { newPassword = it },
                        singleLine = true,
                        label = { Text("New recovery password", color = TextDim) },
                        visualTransformation = androidx.compose.ui.text.input.PasswordVisualTransformation(),
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(Modifier.width(8.dp))
                    Button(
                        onClick = { vm.changePassword(newPassword) { newPassword = "" } },
                        enabled = newPassword.isNotEmpty() && !vm.busy,
                    ) { Text("Change") }
                }
                Text(
                    "Used to sign in on new devices. It never leaves this phone — the server only stores an encrypted bundle.",
                    color = TextMuted, fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                )
            }

            // ---- beta invites (founder or Community Steward)
            if (section == SettingsSection.Invites && canIssueInvites) {
                item {
                    SettingsLabel(if (vm.myProfile?.founder == true) "INVITES" else "COMMUNITY STEWARD")
                    Text(
                        if (vm.myProfile?.founder == true) {
                            "Registration is invite-only. Create a link and share it — each " +
                                "link admits one new account. Tap a link to copy it."
                        } else {
                            "You can invite people you know to the beta and manage your own " +
                                "unused links. This role does not grant moderation or Console access."
                        },
                        color = TextMuted, fontSize = 12.sp,
                        modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                    )
                    OutlinedButton(
                        onClick = { vm.mintInvite() },
                        enabled = !vm.busy,
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                    ) { Text("Create invite link") }
                }
                items(vm.invites, key = { "inv-" + it.code }) { inv ->
                    val context = LocalContext.current
                    val shareText = inv.link.ifEmpty { inv.code }
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .clickable {
                                val cm = context.getSystemService(Context.CLIPBOARD_SERVICE)
                                    as android.content.ClipboardManager
                                cm.setPrimaryClip(
                                    android.content.ClipData.newPlainText("invite", shareText)
                                )
                                android.widget.Toast
                                    .makeText(context, "Invite link copied", android.widget.Toast.LENGTH_SHORT)
                                    .show()
                            }
                            .padding(horizontal = 20.dp, vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(
                                shareText,
                                color = if (inv.usedHandle == null) TextMain else TextMuted,
                                fontSize = if (inv.link.isEmpty()) 14.sp else 12.sp,
                                fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace,
                            )
                            Text(
                                if (inv.usedHandle != null) "used by @${inv.usedHandle}"
                                else "unused · tap to copy link",
                                color = TextMuted, fontSize = 12.sp,
                            )
                        }
                        if (inv.usedHandle == null) {
                            OutlinedButton(onClick = { vm.revokeInvite(inv.code) }) {
                                Text("Revoke", color = Danger)
                            }
                        }
                    }
                }
            }

            // ---- your data
            if (section == SettingsSection.Data) item {
                SettingsLabel("YOUR DATA")
                OutlinedButton(
                    onClick = { vm.exportMyData() },
                    enabled = !vm.busy,
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                ) { Text("Export my data") }
                Text(
                    "Saves everything Connect stores about you — profile, posts, comments, follows — as a JSON file in Downloads.",
                    color = TextMuted, fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                )
            }

            // ---- account
            if (section == SettingsSection.Data) item {
                SettingsLabel("ACCOUNT")
                OutlinedButton(
                    onClick = { onClose(); vm.signOut() },
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                ) { Text("Sign out", color = Danger) }
                Spacer(Modifier.height(8.dp))
                if (!confirmDelete) {
                    OutlinedButton(
                        onClick = { confirmDelete = true },
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                    ) { Text("Delete account", color = Danger) }
                } else {
                    Text(
                        "Confirm by typing your handle and account password.",
                        color = TextMain, fontSize = 13.sp,
                        modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                    )
                    OutlinedTextField(
                        value = deleteHandle,
                        onValueChange = { deleteHandle = it },
                        singleLine = true,
                        label = { Text("Handle", color = TextDim) },
                        prefix = { Text("@", color = TextDim) },
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                    )
                    Spacer(Modifier.height(6.dp))
                    OutlinedTextField(
                        value = deletePassword,
                        onValueChange = { deletePassword = it },
                        singleLine = true,
                        label = { Text("Account password", color = TextDim) },
                        visualTransformation = androidx.compose.ui.text.input.PasswordVisualTransformation(),
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                    )
                    Spacer(Modifier.height(8.dp))
                    Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp)) {
                        OutlinedButton(
                            onClick = { confirmDelete = false; deleteHandle = ""; deletePassword = "" },
                            modifier = Modifier.weight(1f),
                        ) { Text("Cancel") }
                        Spacer(Modifier.width(8.dp))
                        Button(
                            onClick = { onClose(); vm.deleteAccount(deleteHandle, deletePassword) },
                            enabled = deleteHandle.isNotBlank() && !vm.busy,
                            colors = ButtonDefaults.buttonColors(containerColor = Danger),
                            modifier = Modifier.weight(1f),
                        ) { Text("Delete forever") }
                    }
                }
                Text(
                    "Deletion is immediate and permanent: your posts, comments, follows, profile and devices are erased from the server.",
                    color = TextMuted, fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                )
            }
            }
        }
    }

}

/** Manage one owned fold: members, adding from followers, deletion. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FoldManageSheet(vm: ConnectViewModel, foldId: String, onClose: () -> Unit) {
    val fold = vm.folds.find { it.circleId == foldId && it.owned }
    if (fold == null) {
        LaunchedEffect(Unit) { onClose() }
        return
    }
    var confirmDelete by remember { mutableStateOf(false) }
    // People we can resolve to names: your followers and who you follow.
    val known = remember(vm.followers, vm.following) {
        (vm.followers + vm.following).distinctBy { it.accountId }
    }
    ModalBottomSheet(onDismissRequest = onClose, containerColor = Panel) {
        LazyColumn(Modifier.fillMaxWidth().padding(bottom = 32.dp)) {
            item {
                Text(
                    "\uD83D\uDD12 " + fold.name,
                    color = TextMain, fontWeight = FontWeight.Bold, fontSize = 17.sp,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                )
                Text(
                    "Active members can see and publish encrypted Fold activity.",
                    color = TextMuted, fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 20.dp),
                )
                if (!vm.encryptedFoldsReady) {
                    Text(
                        "Membership invitations are ready in Connect and will activate with the pending HIVE update.",
                        color = Honey,
                        fontSize = 12.sp,
                        modifier = Modifier.padding(horizontal = 20.dp, vertical = 6.dp),
                    )
                }
                SettingsLabel("MEMBERS")
                if (fold.members.isEmpty()) {
                    Text(
                        "No members yet — add people below.",
                        color = TextMuted, fontSize = 13.sp,
                        modifier = Modifier.padding(horizontal = 20.dp),
                    )
                }
            }
            items(fold.members.filter { it.accountId != vm.accountId }, key = { "fm-${it.accountId}" }) { member ->
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(member.label, color = TextMain, fontSize = 14.sp)
                        Text("@" + member.handle, color = TextMuted, fontSize = 12.sp)
                    }
                    OutlinedButton(
                        onClick = { vm.foldRemoveMember(fold, member) },
                        enabled = vm.encryptedFoldsReady,
                    ) {
                        Text("Remove", color = Danger)
                    }
                }
            }
            if (fold.pending.isNotEmpty()) item { SettingsLabel("PENDING INVITATIONS") }
            items(fold.pending, key = { "fp-${it.accountId}" }) { member ->
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(member.label, color = TextMain, fontSize = 14.sp)
                        Text("Awaiting acceptance", color = TextMuted, fontSize = 12.sp)
                    }
                    OutlinedButton(
                        onClick = { vm.foldRemoveMember(fold, member) },
                        enabled = vm.encryptedFoldsReady,
                    ) {
                        Text("Cancel")
                    }
                }
            }
            item { SettingsLabel("INVITE FOLLOWERS") }
            val unavailable = (fold.members + fold.pending).map { it.accountId }.toSet()
            val candidates = vm.followers.filter { it.accountId !in unavailable }
            if (candidates.isEmpty()) {
                item {
                    Text(
                        "Every follower is already active or invited.",
                        color = TextMuted, fontSize = 13.sp,
                        modifier = Modifier.padding(horizontal = 20.dp),
                    )
                }
            }
            items(candidates, key = { "fc-" + it.accountId }) { a ->
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(a.label, color = TextMain, fontSize = 14.sp)
                        Text("@" + a.handle, color = TextMuted, fontSize = 12.sp)
                    }
                    OutlinedButton(
                        onClick = { vm.foldInvite(fold, a) },
                        enabled = vm.encryptedFoldsReady,
                    ) {
                        Text("Invite")
                    }
                }
            }
            item {
                SettingsLabel("DANGER ZONE")
                if (!confirmDelete) {
                    OutlinedButton(
                        onClick = { confirmDelete = true },
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
                    ) { Text("Delete fold", color = Danger) }
                } else {
                    Text(
                        "This permanently deletes the fold AND every post shared to it.",
                        color = TextMain, fontSize = 13.sp,
                        modifier = Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                    )
                    Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp)) {
                        OutlinedButton(
                            onClick = { confirmDelete = false },
                            modifier = Modifier.weight(1f),
                        ) { Text("Cancel") }
                        Spacer(Modifier.width(8.dp))
                        Button(
                            onClick = { onClose(); vm.deleteFold(fold) },
                            colors = ButtonDefaults.buttonColors(containerColor = Danger),
                            modifier = Modifier.weight(1f),
                        ) { Text("Delete forever") }
                    }
                }
            }
        }
    }
}

@Composable
private fun SettingsLabel(text: String) {
    Text(
        text,
        color = Accent2, fontSize = 12.sp, fontWeight = FontWeight.Bold,
        letterSpacing = 1.sp,
        modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 18.dp, bottom = 6.dp),
    )
}

@Composable
private fun SettingsNavRow(
    icon: ImageVector,
    title: String,
    summary: String,
    onClick: () -> Unit,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 20.dp, vertical = 13.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier.size(38.dp).clip(RoundedCornerShape(8.dp)).background(PanelHi),
            contentAlignment = Alignment.Center,
        ) {
            Icon(icon, null, tint = Accent2, modifier = Modifier.size(21.dp))
        }
        Spacer(Modifier.width(13.dp))
        Column(Modifier.weight(1f)) {
            Text(title, color = TextMain, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
            Text(summary, color = TextMuted, fontSize = 12.sp, maxLines = 2)
        }
        Icon(Icons.Outlined.ChevronRight, null, tint = TextDim)
    }
    HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 71.dp))
}

@Composable
private fun SettingsSwitch(
    title: String,
    subtitle: String,
    checked: Boolean,
    onChange: (Boolean) -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, color = TextMain, fontSize = 14.sp)
            Text(subtitle, color = TextMuted, fontSize = 12.sp)
        }
        Switch(
            checked = checked,
            onCheckedChange = onChange,
            colors = SwitchDefaults.colors(
                checkedTrackColor = Accent,
                checkedThumbColor = TextMain,
                uncheckedTrackColor = PanelHover,
                uncheckedThumbColor = TextMuted,
                uncheckedBorderColor = Border,
            ),
        )
    }
}

@Composable
private fun CommentPolicyChip(label: String, value: String, vm: ConnectViewModel) {
    val selected = vm.setCommentsFrom == value
    Text(
        label,
        color = if (selected) Accent2 else TextMuted,
        fontSize = 12.sp,
        fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
        maxLines = 1,
        softWrap = false,
        modifier = Modifier
            .clip(RoundedCornerShape(20.dp))
            .background(if (selected) AccentSoft else PanelHi)
            .clickable { vm.saveSettings(vm.setDiscoverable, vm.setAutoAccept, value) }
            .padding(horizontal = 12.dp, vertical = 7.dp),
    )
}

/** A pending follow request: accept / decline, and follow back inline. */
@Composable
fun FollowRequestRow(vm: ConnectViewModel, a: Author, onOpen: () -> Unit) {
    val followingAlready = vm.following.any { it.accountId == a.accountId }
    val requestedAlready = vm.pendingOut.any { it.accountId == a.accountId }
    Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp)) {
        Row(
            Modifier.fillMaxWidth().clickable(onClick = onOpen),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            AuthorAvatar(vm, a, 40.dp)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(a.label, color = TextMain, fontWeight = FontWeight.SemiBold, fontSize = 14.sp)
                Text("@${a.handle}", color = TextMuted, fontSize = 12.sp)
            }
            IconButton(onClick = { vm.ignoreRequest(a) }) {
                Text("\u2715", color = TextMuted, fontSize = 16.sp)
            }
        }
        Spacer(Modifier.height(6.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            val stillPending = vm.pendingIn.any { it.accountId == a.accountId }
            if (stillPending) {
                Button(onClick = { vm.accept(a) }, enabled = !vm.busy) { Text("Accept") }
                Spacer(Modifier.width(8.dp))
                OutlinedButton(onClick = { vm.decline(a) }, enabled = !vm.busy) { Text("Decline") }
            } else {
                Text("Accepted \u2713", color = TextMuted, fontSize = 13.sp)
            }
            Spacer(Modifier.width(8.dp))
            when {
                followingAlready -> OutlinedButton(onClick = { vm.unfollow(a) }) { Text("Following") }
                requestedAlready -> OutlinedButton(onClick = { vm.unfollow(a) }) { Text("Requested") }
                else -> OutlinedButton(onClick = { vm.requestFollow(a.accountId) }) {
                    Text("Follow back", color = Accent2)
                }
            }
        }
    }
}

@Composable
fun StatCell(number: String, label: String, onClick: () -> Unit) {
    Column(
        Modifier
            .clickable(onClick = onClick)
            .padding(horizontal = 18.dp, vertical = 4.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(number, color = TextMain, fontWeight = FontWeight.Bold, fontSize = 17.sp)
        Text(label, color = TextDim, fontSize = 12.sp)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FollowListSheet(vm: ConnectViewModel, which: String, onClose: () -> Unit) {
    val list = if (which == "followers") vm.followers else vm.following
    ModalBottomSheet(onDismissRequest = onClose, containerColor = Panel) {
        Text(
            if (which == "followers") "Followers" else "Following",
            color = TextMain, fontWeight = FontWeight.Bold, fontSize = 17.sp,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
        )
        LazyColumn(Modifier.heightIn(max = 480.dp)) {
            if (list.isEmpty()) {
                item { Text("Nobody here yet.", color = TextDim, modifier = Modifier.padding(16.dp)) }
            }
            items(list, key = { it.accountId }) { a ->
                AuthorRow(vm, a, onOpen = { onClose(); vm.openProfile(a.accountId) }) {
                    if (which == "following") {
                        OutlinedButton(onClick = { vm.unfollow(a) }) { Text("Unfollow") }
                    } else {
                        OutlinedButton(onClick = { vm.block(a) }) { Text("Block") }
                    }
                }
            }
            item { Spacer(Modifier.height(24.dp)) }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EditProfileSheet(vm: ConnectViewModel, p: Profile, onClose: () -> Unit) {
    var name by remember { mutableStateOf(p.displayName) }
    var bio by remember { mutableStateOf(p.bio) }
    var avatarUri by remember { mutableStateOf<Uri?>(null) }
    val pickAvatar = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri -> if (uri != null) avatarUri = uri }

    ModalBottomSheet(onDismissRequest = onClose, containerColor = Panel) {
        Column(
            Modifier.fillMaxWidth().padding(horizontal = 20.dp).padding(bottom = 32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text(
                "Edit profile",
                color = TextMain, fontWeight = FontWeight.Bold, fontSize = 17.sp,
            )
            Spacer(Modifier.height(14.dp))
            Box(
                Modifier.clickable {
                    pickAvatar.launch(
                        PickVisualMediaRequest(
                            ActivityResultContracts.PickVisualMedia.ImageOnly,
                        ),
                    )
                },
            ) {
                if (avatarUri != null) {
                    AsyncImage(
                        model = avatarUri,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier.size(88.dp).clip(CircleShape),
                    )
                } else {
                    Avatar(p.avatarBlob, p.handle, 88.dp, vm)
                }
                Text(
                    "✎", color = Bg, fontSize = 13.sp,
                    modifier = Modifier
                        .align(Alignment.BottomEnd)
                        .clip(CircleShape)
                        .background(BrandBrush)
                        .padding(5.dp),
                )
            }
            Spacer(Modifier.height(14.dp))
            OutlinedTextField(
                value = name, onValueChange = { name = it },
                label = { Text("Display name") }, singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(10.dp))
            OutlinedTextField(
                value = bio, onValueChange = { bio = it },
                label = { Text("Bio") }, maxLines = 4,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(16.dp))
            Button(
                onClick = {
                    vm.saveProfile(name, bio, avatarUri)
                    onClose()
                },
                enabled = !vm.busy,
                modifier = Modifier.fillMaxWidth().height(48.dp),
            ) { Text("Save") }
        }
    }
}

// ---------------------------------------------------------------- composer

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ComposerSheet(vm: ConnectViewModel, onClose: () -> Unit) {
    val initialDraft = remember { vm.loadPostDraft() }
    var body by remember { mutableStateOf(initialDraft.body) }
    var audience by remember { mutableStateOf(initialDraft.audience) }
    var attachments by remember { mutableStateOf(listOf<PendingPostMedia>()) }
    var altText by remember { mutableStateOf(initialDraft.altText) }
    var contentWarning by remember { mutableStateOf(initialDraft.contentWarning) }
    val context = LocalContext.current
    LaunchedEffect(Unit) { vm.refreshFolds() }
    LaunchedEffect(body, audience, altText, contentWarning) {
        kotlinx.coroutines.delay(500)
        vm.savePostDraft(PostDraft(body, audience, altText, contentWarning))
    }
    val pickImages = rememberLauncherForActivityResult(
        ActivityResultContracts.PickMultipleVisualMedia(10),
    ) { uris ->
        if (uris.isNotEmpty()) {
            attachments = (attachments.filterNot { it.mime.startsWith("video/") } +
                uris.map { PendingPostMedia(it, "image/jpeg") }).take(10)
        }
    }
    val pickVideo = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            val mime = context.contentResolver.getType(uri)?.takeIf { it.startsWith("video/") }
                ?: "video/mp4"
            attachments = listOf(PendingPostMedia(uri, mime))
        }
    }
    var captureUri by remember { mutableStateOf<Uri?>(null) }
    val takePhoto = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicture(),
    ) { ok ->
        val uri = captureUri
        if (ok && uri != null) {
            attachments = (attachments.filterNot { it.mime.startsWith("video/") } +
                PendingPostMedia(uri, "image/jpeg")).take(10)
        }
        captureUri = null
    }
    var captureVideoUri by remember { mutableStateOf<Uri?>(null) }
    val takeVideo = rememberLauncherForActivityResult(
        ActivityResultContracts.CaptureVideo(),
    ) { ok ->
        val uri = captureVideoUri
        if (ok && uri != null) attachments = listOf(PendingPostMedia(uri, "video/mp4"))
        captureVideoUri = null
    }

    ModalBottomSheet(onDismissRequest = onClose, containerColor = Panel) {
        Column(Modifier.fillMaxWidth().padding(bottom = 28.dp)) {
            // ---- header: close | title | gradient Post pill
            val canPost = (body.isNotBlank() || attachments.isNotEmpty()) && !vm.busy
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "✕", color = TextDim, fontSize = 18.sp,
                    modifier = Modifier.clip(CircleShape).clickable(onClick = onClose)
                        .padding(horizontal = 8.dp, vertical = 4.dp),
                )
                Spacer(Modifier.width(8.dp))
                Text("New post", color = TextMain, fontWeight = FontWeight.Bold, fontSize = 17.sp)
                Spacer(Modifier.weight(1f))
                Box(
                    Modifier
                        .clip(RoundedCornerShape(20.dp))
                        .background(if (canPost) BrandBrush else SolidColor(PanelHover))
                        .clickable(enabled = canPost) {
                            vm.createPost(
                                body,
                                audience,
                                attachments,
                                altText,
                                contentWarning,
                                onDone = {
                                    vm.clearPostDraft()
                                    onClose()
                                },
                            )
                        }
                        .padding(horizontal = 20.dp, vertical = 9.dp),
                ) {
                    Text(
                        if (vm.busy) vm.status.ifEmpty { "Posting…" } else "Post",
                        color = if (canPost) TextMain else TextMuted,
                        fontWeight = FontWeight.Bold, fontSize = 14.sp,
                    )
                }
            }
            Spacer(Modifier.height(14.dp))

            // ---- who's posting + audience toggle
            val myName = vm.myProfile?.displayName?.ifEmpty { null } ?: "@${vm.handle}"
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Avatar(vm.myProfile?.avatarBlob, myName, 40.dp, vm)
                Spacer(Modifier.width(10.dp))
                Column {
                    Text(myName, color = TextMain, fontWeight = FontWeight.SemiBold, fontSize = 14.sp)
                    Spacer(Modifier.height(5.dp))
                    Row(
                        Modifier
                            .horizontalScroll(rememberScrollState())
                            .clip(RoundedCornerShape(20.dp))
                            .background(PanelHi)
                            .padding(3.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        AudienceSegment("👥 Followers", audience == "followers") { audience = "followers" }
                        AudienceSegment("🌐 Public", audience == "public") { audience = "public" }
                    }
                }
            }

            // ---- the writing area (borderless)
            TextField(
                value = body, onValueChange = { body = it },
                placeholder = {
                    Text(
                        "What's happening on your side of the fence?",
                        color = TextMuted, fontSize = 16.sp,
                    )
                },
                colors = TextFieldDefaults.colors(
                    focusedContainerColor = Color.Transparent,
                    unfocusedContainerColor = Color.Transparent,
                    focusedIndicatorColor = Color.Transparent,
                    unfocusedIndicatorColor = Color.Transparent,
                    cursorColor = Accent2,
                    focusedTextColor = TextMain,
                    unfocusedTextColor = TextMain,
                ),
                textStyle = LocalTextStyle.current.copy(fontSize = 16.sp, lineHeight = 23.sp),
                minLines = 4, maxLines = 10,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp),
            )
            OutlinedTextField(
                value = altText,
                onValueChange = { altText = it.take(2_000) },
                label = { Text("Alt text for media") },
                placeholder = { Text("Describe what is visible or audible") },
                enabled = attachments.isNotEmpty(),
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            )
            Spacer(Modifier.height(8.dp))
            OutlinedTextField(
                value = contentWarning,
                onValueChange = { contentWarning = it.take(200) },
                label = { Text("Content warning (optional)") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            )
            Spacer(Modifier.height(10.dp))

            // ---- attached media
            if (attachments.isNotEmpty()) {
                LazyRow(contentPadding = PaddingValues(horizontal = 16.dp)) {
                    itemsIndexed(attachments) { i, attachment ->
                        Box(Modifier.padding(end = 10.dp)) {
                            if (attachment.mime.startsWith("image/")) {
                                AsyncImage(
                                    model = attachment.uri,
                                    contentDescription = altText.ifBlank { null },
                                    contentScale = ContentScale.Crop,
                                    modifier = Modifier
                                        .size(104.dp)
                                        .clip(RoundedCornerShape(14.dp))
                                        .border(1.dp, BorderSoft, RoundedCornerShape(14.dp)),
                                )
                            } else {
                                Surface(
                                    color = PanelHi,
                                    shape = RoundedCornerShape(14.dp),
                                    border = BorderStroke(1.dp, BorderSoft),
                                    modifier = Modifier.size(104.dp),
                                ) {
                                    Column(
                                        horizontalAlignment = Alignment.CenterHorizontally,
                                        verticalArrangement = Arrangement.Center,
                                    ) {
                                        Icon(Icons.Outlined.Videocam, null, tint = Accent2)
                                        Text("Video", color = TextDim, fontSize = 12.sp)
                                    }
                                }
                            }
                            Text(
                                "✕", color = TextMain, fontSize = 12.sp,
                                modifier = Modifier
                                    .align(Alignment.TopEnd)
                                    .padding(5.dp)
                                    .clip(CircleShape)
                                    .background(Color(0xCC0A0C11))
                                    .clickable {
                                        attachments = attachments.filterIndexed { j, _ -> j != i }
                                    }
                                    .padding(horizontal = 7.dp, vertical = 4.dp),
                            )
                        }
                    }
                }
                Spacer(Modifier.height(12.dp))
            }

            // ---- media toolbar
            HorizontalDivider(color = BorderSoft)
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                ComposerTool(Icons.Outlined.Image) {
                    pickImages.launch(
                        PickVisualMediaRequest(
                            ActivityResultContracts.PickVisualMedia.ImageOnly,
                        ),
                    )
                }
                Spacer(Modifier.width(10.dp))
                ComposerTool(Icons.Outlined.PhotoCamera) {
                    val uri = newCaptureUri(context)
                    captureUri = uri
                    takePhoto.launch(uri)
                }
                Spacer(Modifier.width(10.dp))
                ComposerTool(Icons.Outlined.Movie) {
                    pickVideo.launch(
                        PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.VideoOnly),
                    )
                }
                Spacer(Modifier.width(10.dp))
                ComposerTool(Icons.Outlined.Videocam) {
                    val uri = newVideoCaptureUri(context)
                    captureVideoUri = uri
                    takeVideo.launch(uri)
                }
                Spacer(Modifier.weight(1f))
                if (attachments.isNotEmpty()) {
                    Text(
                        if (attachments.any { it.mime.startsWith("video/") }) "1 video"
                        else "${attachments.size}/10 photos",
                        color = TextMuted,
                        fontSize = 12.sp,
                    )
                }
            }
        }
    }
}

/** One half of the followers/public pill toggle in the composer. */
@Composable
private fun AudienceSegment(label: String, selected: Boolean, onClick: () -> Unit) {
    Text(
        label,
        color = if (selected) Accent2 else TextMuted,
        fontSize = 12.sp,
        fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
        modifier = Modifier
            .clip(RoundedCornerShape(20.dp))
            .background(if (selected) AccentSoft else Color.Transparent)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 5.dp),
    )
}

/** Round media button in the composer toolbar. */
@Composable
private fun ComposerTool(icon: androidx.compose.ui.graphics.vector.ImageVector, onClick: () -> Unit) {
    Box(
        Modifier.size(40.dp).clip(CircleShape).background(PanelHi).clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) { Icon(icon, null, tint = TextDim, modifier = Modifier.size(20.dp)) }
}

// ------------------------------------------------------------------ legal

/** Full-screen scrollable viewer for a served legal document. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LegalDocViewerDialog(
    vm: ConnectViewModel,
    doc: String,
    onClose: () -> Unit,
    serverOverride: String? = null,
) {
    var text by remember(doc) { mutableStateOf<String?>(null) }
    var error by remember(doc) { mutableStateOf<String?>(null) }
    LaunchedEffect(doc) {
        runCatching { vm.fetchLegalDocument(doc, serverOverride) }
            .onSuccess { text = it }
            .onFailure { error = it.message ?: "could not load document" }
    }
    Dialog(
        onDismissRequest = onClose,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Surface(Modifier.fillMaxSize(), color = Panel) {
            Column(
                Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing),
            ) {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 2.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    IconButton(onClick = onClose) {
                        Icon(Icons.Outlined.Close, "Close", tint = TextDim)
                    }
                    Text(
                        if (doc == "privacy") "Privacy Policy" else "Terms of Service",
                        color = TextMain, fontWeight = FontWeight.Bold, fontSize = 18.sp,
                    )
                }
                HorizontalDivider(color = BorderSoft)
                when {
                    error != null -> Text(
                        error ?: "",
                        color = Danger,
                        modifier = Modifier.padding(24.dp).fillMaxWidth(),
                        textAlign = TextAlign.Center,
                    )
                    text == null -> Box(
                        Modifier.fillMaxWidth().padding(top = 48.dp),
                        contentAlignment = Alignment.Center,
                    ) { CircularProgressIndicator(color = Accent2) }
                    else -> Text(
                        text ?: "",
                        color = TextDim, fontSize = 13.sp,
                        fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace,
                        modifier = Modifier
                            .weight(1f)
                            .verticalScroll(rememberScrollState())
                            .padding(horizontal = 16.dp, vertical = 12.dp),
                    )
                }
            }
        }
    }
}

/**
 * Blocking review gate (§16): when the served Terms/Privacy Policy versions
 * are newer than what this account accepted, the whole app is gated behind
 * explicit review + acceptance. Back is consumed; the only ways forward are
 * Accept or Sign out.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LegalGateSheet(vm: ConnectViewModel) {
    var viewingDoc by remember { mutableStateOf<String?>(null) }
    androidx.activity.compose.BackHandler { /* gate: consume */ }
    Surface(Modifier.fillMaxSize(), color = Bg) {
        Column(
            Modifier
                .fillMaxSize()
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(horizontal = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Spacer(Modifier.height(48.dp))
            Box(
                Modifier.size(64.dp).clip(CircleShape).background(Accent2.copy(alpha = 0.16f)),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Outlined.Description, null,
                    tint = Accent2, modifier = Modifier.size(32.dp),
                )
            }
            Spacer(Modifier.height(20.dp))
            Text(
                "Updated terms",
                color = TextMain, fontWeight = FontWeight.Bold, fontSize = 22.sp,
            )
            Spacer(Modifier.height(10.dp))
            Text(
                "The Terms of Service and Privacy Policy have been updated. " +
                    "Please review and accept them to keep using Connect.",
                color = TextDim, fontSize = 14.sp, textAlign = TextAlign.Center,
            )
            Spacer(Modifier.height(24.dp))
            vm.legalDocs.forEach { d ->
                Surface(
                    color = Panel.copy(alpha = 0.86f),
                    shape = HiveCutShape,
                    border = BorderStroke(1.dp, if (d.accepted) BorderSoft else Accent2.copy(alpha = 0.4f)),
                    modifier = Modifier.fillMaxWidth().padding(vertical = 5.dp),
                ) {
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .clickable { viewingDoc = d.doc }
                            .padding(14.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(
                                if (d.doc == "privacy") "Privacy Policy" else "Terms of Service",
                                color = TextMain, fontWeight = FontWeight.SemiBold, fontSize = 15.sp,
                            )
                            Text(
                                "Version ${d.version}" + if (d.accepted) " · accepted" else " · tap to read",
                                color = TextMuted, fontSize = 12.sp,
                            )
                        }
                        Icon(Icons.Outlined.ChevronRight, null, tint = TextDim)
                    }
                }
            }
            Spacer(Modifier.height(24.dp))
            Button(
                onClick = { vm.acceptLegalDocuments() },
                enabled = !vm.legalAccepting,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (vm.legalAccepting) "Recording…" else "I have reviewed both — Accept")
            }
            Spacer(Modifier.height(8.dp))
            TextButton(onClick = { vm.signOut() }) {
                Text("Sign out instead", color = TextMuted)
            }
            Spacer(Modifier.height(24.dp))
        }
    }
    viewingDoc?.let { doc ->
        LegalDocViewerDialog(vm, doc, onClose = { viewingDoc = null })
    }
}
