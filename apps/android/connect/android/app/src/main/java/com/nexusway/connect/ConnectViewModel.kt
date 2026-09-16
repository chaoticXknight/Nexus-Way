// Owns visible Connect state and coordinates HIVE calls for every screen.
// It does not own durable credentials (Store), wire formats (HiveClient), or
// persistent background delivery (the separate Nexus Notify application).

package com.nexusway.connect

import android.app.Application
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Build
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.security.SecureRandom
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/** org.json's optString turns JSON null into the literal string "null";
 * this gives an honest Kotlin null for absent/null/empty fields. */
private fun JSONObject.strOrNull(key: String): String? =
    if (isNull(key)) null else optString(key, "").ifEmpty { null }

private const val NOTIFIER_PACKAGE = "com.nexusway.notify"

data class Author(
    val accountId: String,
    val handle: String,
    val displayName: String,
    val avatarBlob: String? = null,
) {
    val label: String get() = displayName.ifEmpty { "@$handle" }

    companion object {
        fun of(v: JSONObject) = Author(
            v.optString("account_id"),
            v.optString("handle"),
            v.optString("display_name"),
            v.strOrNull("avatar_blob"),
        )
    }
}

data class Post(
    val postId: String,
    val author: Author,
    val created: Long,
    val kind: String,
    val body: String,
    val media: List<String>,
    val mediaTypes: List<String>,
    val altText: String,
    val contentWarning: String,
    val audience: String,
    val reactions: Long,
    val myReaction: String?,
    val comments: Long,
    val edited: Long? = null,
    val pinned: Boolean = false,
    val saved: Boolean = false,
    val encryptedBody: String? = null,
)

data class PostRevision(val body: String, val replacedAt: Long)

data class PendingPostMedia(val uri: Uri, val mime: String)

data class Comment(
    val commentId: String,
    val author: Author,
    val body: String,
    val created: Long,
    val parentId: String?,
    val reactions: Long,
    val myReaction: String?,
    val edited: Long? = null,
)

data class Alert(
    val id: String,
    val kind: String,
    val from: Author,
    val subjectId: String,
    val created: Long,
    val seen: Boolean,
    /** Announcement content, present only for kind == "system". */
    val title: String = "",
    val body: String = "",
    val announceKind: String = "",
    val foldId: String = "",
)

/** One legal document's version + acceptance state (drives the review gate). */
data class LegalDoc(
    val doc: String,
    val version: String,
    val accepted: Boolean,
)

data class Profile(
    val accountId: String,
    val handle: String,
    val displayName: String,
    val bio: String,
    val avatarBlob: String?,
    val founder: Boolean,
    val communityRole: String,
    val membershipTier: String,
    /** null | "requested" | "accepted" — MY follow state toward them. */
    val followState: String?,
    val followers: Long,
    val following: Long,
    val postCount: Long,
)

/** A found account in Discover. */
data class SearchHit(val author: Author, val followState: String?)

/** A device enrolled on this account (settings → Devices). */
data class DeviceRow(val id: String, val name: String, val lastSeen: Long, val revoked: Boolean)

/** A Fold: a member-only group you post to (server: "circle"). */
data class Fold(
    val circleId: String,
    val name: String,
    val owned: Boolean,
    val members: List<Author> = listOf(),
    val pending: List<Author> = listOf(),
    val ownerLabel: String = "",
    val keyEpoch: Long = 1,
    val wrappedKey: JSONObject? = null,
    val wrappedKeys: JSONObject? = null,
    val invited: Boolean = false,
    val joinedAt: Long? = null,
)

data class SupportMessage(
    val id: String,
    val senderRole: String,
    val body: String,
    val created: Long,
)

data class SupportThread(
    val id: String,
    val category: String,
    val subject: String,
    val status: String,
    val updated: Long,
    val messages: List<SupportMessage>,
)

/** An invite code row (founder-only settings section). */
data class InviteRow(
    val code: String,
    val link: String,
    val created: Long,
    val usedHandle: String?,
    val usedAt: Long?,
)

data class DirectMessage(
    val id: String,
    val peerAccount: String,
    val peerHandle: String,
    val body: String,
    val sent: Long,
    val mine: Boolean,
    val status: String,
    val attachment: MessageAttachment? = null,
)

data class ChatContact(val author: Author, val direction: String?)

class ConnectViewModel(app: Application) : AndroidViewModel(app) {
    private val store = Store(app)
    private val calls: SecureCallManager? get() = CallSession.current
    private var enrollment: Enrollment? = null
    private var refreshJob: Job? = null
    private var streamEventsJob: Job? = null
    private var foregroundActive = false
    private val messageMediaDirectory = File(app.cacheDir, "wire-media").apply {
        deleteRecursively()
        mkdirs()
    }
    private val postMediaDirectory = File(app.cacheDir, "post-media").apply {
        deleteRecursively()
        mkdirs()
    }
    var messageImages by mutableStateOf<Map<String, ByteArray>>(emptyMap())
        private set
    var messageMediaFiles by mutableStateOf<Map<String, File>>(emptyMap())
        private set
    var postVideoFiles by mutableStateOf<Map<String, File>>(emptyMap())
        private set
    var foldMediaFiles by mutableStateOf<Map<String, File>>(emptyMap())
        private set

    /** Exposed so the UI can build a Coil ImageLoader on the same trust +
     * auth plumbing; null until signed in. */
    var api by mutableStateOf<HiveClient?>(null); private set

    var enrolled by mutableStateOf(false); private set
    var handle by mutableStateOf(""); private set
    var accountId by mutableStateOf(""); private set
    var status by mutableStateOf(""); private set
    var busy by mutableStateOf(false); private set
    var themeMode by mutableStateOf(store.themeMode); private set
    var autoAcceptMessageInvites by mutableStateOf(store.autoAcceptMessageInvites); private set
    val callState: CallUiState get() = calls?.state ?: CallUiState()
    val callEglContext get() = requireNotNull(calls).eglContext
    val shouldRequestNotificationPermission: Boolean
        get() = !store.notificationPermissionAsked

    /** Loud failures (enrollment, linking) — shown as a dialog. */
    var errorDialog by mutableStateOf<String?>(null)

    // Home.
    var posts by mutableStateOf(listOf<Post>()); private set
    var nextCursor by mutableStateOf<String?>(null); private set
    var feedMode by mutableStateOf("home"); private set
    private var feedQuery = ""
    var postRevisions by mutableStateOf(listOf<PostRevision>()); private set
    var revisionsPostId by mutableStateOf<String?>(null); private set

    // Graph.
    var following by mutableStateOf(listOf<Author>()); private set
    var followers by mutableStateOf(listOf<Author>()); private set
    var pendingIn by mutableStateOf(listOf<Author>()); private set
    var pendingOut by mutableStateOf(listOf<Author>()); private set

    /** Requests accepted this visit — kept visible so the user can still Follow back. */
    var recentlyAccepted by mutableStateOf(listOf<Author>()); private set

    /** Requests dismissed with ✕ — hidden locally without declining. */
    var ignoredRequests by mutableStateOf(setOf<String>()); private set

    /** Follow-request rows to show: pending (minus ignored) plus just-accepted ones. */
    val requestRows: List<Author>
        get() = pendingIn.filter { it.accountId !in ignoredRequests } +
            recentlyAccepted.filter { ra ->
                ra.accountId !in ignoredRequests && pendingIn.none { it.accountId == ra.accountId }
            }

    fun ignoreRequest(a: Author) {
        ignoredRequests = ignoredRequests + a.accountId
    }

    fun changeThemeMode(mode: String) {
        themeMode = if (mode == "light") "light" else "dark"
        store.themeMode = themeMode
    }

    var autoUpdateEnabled by mutableStateOf(store.autoUpdate); private set

    fun changeAutoUpdate(enabled: Boolean) {
        autoUpdateEnabled = enabled
        store.autoUpdate = enabled
    }

    fun markNotificationPermissionAsked() {
        store.notificationPermissionAsked = true
    }

    fun changeAutoAcceptMessageInvites(enabled: Boolean) {
        autoAcceptMessageInvites = enabled
        store.autoAcceptMessageInvites = enabled
        if (enabled) viewModelScope.launch { runCatching { refreshChatStates() } }
    }

    // Alerts.
    var alerts by mutableStateOf(listOf<Alert>()); private set

    /** True while the account must review + accept updated ToS/PP before
     *  continuing (§16). Blocks the whole UI behind the review gate. */
    var legalGateNeeded by mutableStateOf(false); private set
    var legalDocs by mutableStateOf(listOf<LegalDoc>()); private set
    var legalAccepting by mutableStateOf(false); private set
    var unseen by mutableStateOf(0L); private set

    // Discover.
    var searchResults by mutableStateOf(listOf<SearchHit>()); private set
    var searching by mutableStateOf(false); private set

    // Profile being viewed (someone else, as an overlay).
    var viewedProfile by mutableStateOf<Profile?>(null); private set
    var viewedPosts by mutableStateOf(listOf<Post>()); private set

    // My own profile (Profile tab header + edit prefill).
    var myProfile by mutableStateOf<Profile?>(null); private set
    var myPosts by mutableStateOf(listOf<Post>()); private set

    // Comments sheet.
    var openPostId by mutableStateOf<String?>(null); private set
    var dmOpen by mutableStateOf(false)
    var directMessages by mutableStateOf(listOf<DirectMessage>()); private set
        var messageHistorySyncing by mutableStateOf(false); private set
        var messageHistoryDeviceCount by mutableStateOf(0); private set
        var messageHistoryLastSync by mutableStateOf(store.lastMessageHistorySync); private set
        private var historySyncJob: Job? = null
    var chatStates by mutableStateOf(listOf<ChatContact>()); private set
    private var hiddenConversations by mutableStateOf(store.hiddenConversations)
    val messageContacts: List<ChatContact>
        get() {
            val states = chatStates.associateBy { it.author.accountId }
            return (following + followers)
                .distinctBy { it.accountId }
                .map { author ->
                    if (author.accountId in hiddenConversations) ChatContact(author, null)
                    else states[author.accountId] ?: ChatContact(author, null)
                }
                .sortedWith(compareBy<ChatContact> {
                    when (it.direction) {
                        "incoming" -> 0
                        "accepted" -> 1
                        "outgoing" -> 2
                        else -> 3
                    }
                }.thenBy { it.author.label.lowercase() })
        }
    val knownMentionHandles: Set<String>
        get() = buildSet {
            add(handle)
            addAll(messageContacts.map { it.author.handle })
            addAll(searchResults.map { it.author.handle })
            addAll((posts + foldPosts + myPosts + viewedPosts).map { it.author.handle })
            addAll(openComments.map { it.author.handle })
            folds.forEach { fold -> addAll(fold.members.map { it.handle }) }
            myProfile?.handle?.let(::add)
            viewedProfile?.handle?.let(::add)
        }.filterTo(mutableSetOf(), String::isNotBlank)
    var openComments by mutableStateOf(listOf<Comment>()); private set

    // Post detail overlay (opened from alerts / permalinks).
    var detailPost by mutableStateOf<Post?>(null); private set

    // Folds (member-only groups; server "circles").
    var folds by mutableStateOf(listOf<Fold>()); private set
    var foldsOpen by mutableStateOf(false)
    var requestedFoldId by mutableStateOf<String?>(null); private set
    var foldPosts by mutableStateOf(listOf<Post>()); private set
    var foldNextCursor by mutableStateOf<String?>(null); private set
    var encryptedFoldsReady by mutableStateOf(false); private set

    var supportOpen by mutableStateOf(false)
    var supportThreads by mutableStateOf(listOf<SupportThread>()); private set

    // Beta invites (founder or Community Steward; issuer-scoped).
    var invites by mutableStateOf(listOf<InviteRow>()); private set

    // Settings sheet.
    var settingsOpen by mutableStateOf(false)
    var setDiscoverable by mutableStateOf(true); private set
    var setAutoAccept by mutableStateOf(false); private set
    var setCommentsFrom by mutableStateOf("viewers"); private set
    var blockedUsers by mutableStateOf(listOf<Author>()); private set
    var deviceRows by mutableStateOf(listOf<DeviceRow>()); private set

    // Device-link flow (this phone joining an existing account).
    var linkCode by mutableStateOf<String?>(null); private set
    var linkWaiting by mutableStateOf(false); private set

    // In-app updates: the server's APK sha256 vs our own installed base.apk.
    var updateAvailable by mutableStateOf(false); private set
    var updateBusy by mutableStateOf(false); private set
    var updateVersion by mutableStateOf(""); private set
    var updateSeverity by mutableStateOf("normal"); private set
    var updateTitle by mutableStateOf("Update available"); private set
    var updateNotes by mutableStateOf(listOf<String>()); private set
    var updateDownloadedBytes by mutableStateOf(0L); private set
    var updateTotalBytes by mutableStateOf(0L); private set
    var updateBytesPerSecond by mutableStateOf(0L); private set
    var updateEtaSeconds by mutableStateOf<Long?>(null); private set
    var notifierInstallAvailable by mutableStateOf(false); private set
    var notifierInstalled by mutableStateOf(false); private set
    var notifierBusy by mutableStateOf(false); private set
    var notifierDownloadedBytes by mutableStateOf(0L); private set
    var notifierTotalBytes by mutableStateOf(0L); private set
    private var notifierInstallObserved: Boolean? = null

    init {
        store.load()?.let { e ->
            directMessages = store.loadDirectMessages()
            enrollment = e
            enrolled = true
            handle = e.handle
            accountId = e.accountId
            signIn(e)
        }
    }

    private fun run(label: String, loud: Boolean = false, block: suspend () -> Unit) {
        viewModelScope.launch {
            busy = true
            try {
                block()
            } catch (e: Exception) {
                if (e is kotlinx.coroutines.CancellationException) throw e
                val msg = e.message ?: label
                if (loud) errorDialog = msg else status = msg
            } finally {
                busy = false
            }
        }
    }

    // ------------------------------------------------------------- updates

    /** Compare the server APK's sha256 with our own installed base.apk —
     * a mismatch means a new build was deployed. Silent on any failure
     * (older servers don't have the endpoint). */
    private suspend fun checkForUpdate(c: HiveClient) {
        runCatching {
            val version = c.appVersion()
            val server = version.optString("sha256", "")
            if (server.isEmpty()) return
            val own = withContext(Dispatchers.IO) {
                sha256hexFile(java.io.File(getApplication<Application>().applicationInfo.sourceDir))
            }
            val release = version.optJSONObject("release")
            updateVersion = release?.optString("version", "") ?: ""
            updateAvailable = own != server && (
                updateVersion.isBlank() || isNewerVersion(updateVersion, BuildConfig.VERSION_NAME)
            )
            if (updateAvailable) {
                updateSeverity = release?.optString("severity", "normal") ?: "normal"
                updateTitle = release?.optString("title", "Update available") ?: "Update available"
                val notes = release?.optJSONArray("notes")
                updateNotes = (0 until (notes?.length() ?: 0)).mapNotNull { i ->
                    notes?.optString(i)?.takeIf { it.isNotBlank() }
                }
            }
            val notifier = version.optJSONObject("notifier")
            val notifierHash = notifier?.optString("sha256", "").orEmpty()
            if (notifierHash.isNotEmpty()) {
                val installedHash = withContext(Dispatchers.IO) {
                    installedApkHash(NOTIFIER_PACKAGE)
                }
                val wasInstalled = notifierInstallObserved
                notifierInstalled = installedHash != null
                notifierInstallObserved = notifierInstalled
                notifierInstallAvailable = installedHash != notifierHash
                if (wasInstalled == false && notifierInstalled) {
                    enrollment?.let { NexusNotifier.provision(getApplication(), it, c) }
                }
            } else {
                notifierInstallAvailable = false
            }
            // Commit Notify first and Connect last. PackageInstaller owns both
            // sessions after commit, so the Connect process may safely restart.
            if (autoUpdateEnabled && !BuildConfig.LOCAL_RELIABILITY_TEST && !updateBusy && !notifierBusy &&
                callState.phase == CallPhase.IDLE &&
                (updateAvailable || notifierInstallAvailable)
            ) {
                installUpdate()
            }
        }
    }

    private fun isNewerVersion(candidate: String, installed: String): Boolean {
        val candidateParts = candidate.substringBefore('-').split('.').map { it.toIntOrNull() ?: 0 }
        val installedParts = installed.substringBefore('-').split('.').map { it.toIntOrNull() ?: 0 }
        val width = maxOf(candidateParts.size, installedParts.size)
        for (index in 0 until width) {
            val candidatePart = candidateParts.getOrElse(index) { 0 }
            val installedPart = installedParts.getOrElse(index) { 0 }
            if (candidatePart != installedPart) return candidatePart > installedPart
        }
        return false
    }

    /** Download and verify every pending Android artifact, commit Nexus Notify
     * first, then commit Connect last because a successful self-update restarts us. */
    fun installUpdate() {
        if (updateBusy || notifierBusy) return
        val c = api ?: return
        run("update failed", loud = true) {
            updateBusy = true
            updateDownloadedBytes = 0L
            updateTotalBytes = 0L
            updateBytesPerSecond = 0L
            updateEtaSeconds = null
            try {
                val ctx = getApplication<Application>()
                val dir = java.io.File(ctx.cacheDir, "updates").apply { mkdirs() }
                val version = c.appVersion()
                val notifierExpected = version.optJSONObject("notifier")
                    ?.optString("sha256", "").orEmpty()
                val notifierInstalledHash = withContext(Dispatchers.IO) {
                    installedApkHash(NOTIFIER_PACKAGE)
                }
                if (notifierExpected.isNotEmpty() && notifierInstalledHash != notifierExpected) {
                    notifierBusy = true
                    notifierDownloadedBytes = 0L
                    notifierTotalBytes = 0L
                    val notifierApk = java.io.File(dir, "nexus-notify.apk")
                    c.downloadNotifierApk(notifierApk) { downloaded, total ->
                        notifierDownloadedBytes = downloaded
                        notifierTotalBytes = total
                    }
                    val notifierGot = withContext(Dispatchers.IO) { sha256hexFile(notifierApk) }
                    if (notifierGot != notifierExpected) {
                        notifierApk.delete()
                        throw HiveException("Nexus Notify update download corrupted — try again")
                    }
                    withContext(Dispatchers.IO) { verifyNotifierArchive(notifierApk) }
                    withContext(Dispatchers.IO) {
                        UpdateInstaller.commit(ctx, notifierApk, NOTIFIER_PACKAGE)
                    }
                    notifierBusy = false
                    status = "Nexus Notify update installing…"
                }
                val connectExpected = version.optString("sha256", "")
                val connectInstalledHash = withContext(Dispatchers.IO) {
                    installedApkHash(ctx.packageName)
                }
                if (connectExpected.isNotEmpty() && connectInstalledHash != connectExpected) {
                    val connectApk = java.io.File(dir, "nexus-connect.apk")
                    val started = android.os.SystemClock.elapsedRealtime()
                    c.downloadApk(connectApk) { downloaded, total ->
                        val elapsedMs = (android.os.SystemClock.elapsedRealtime() - started).coerceAtLeast(1L)
                        val bytesPerSecond = downloaded * 1000L / elapsedMs
                        updateDownloadedBytes = downloaded
                        updateTotalBytes = total
                        updateBytesPerSecond = bytesPerSecond
                        updateEtaSeconds = if (total > downloaded && bytesPerSecond > 0L) {
                            ((total - downloaded) + bytesPerSecond - 1L) / bytesPerSecond
                        } else {
                            null
                        }
                    }
                    val connectGot = withContext(Dispatchers.IO) { sha256hexFile(connectApk) }
                    if (connectGot != connectExpected) {
                        connectApk.delete()
                        throw HiveException("update download corrupted — try again")
                    }
                    withContext(Dispatchers.IO) {
                        UpdateInstaller.commit(ctx, connectApk, ctx.packageName)
                    }
                    status = "Connect and Nexus Notify updates installing…"
                }
            } finally {
                updateBusy = false
                notifierBusy = false
                updateEtaSeconds = null
            }
        }
    }

    fun installNotifier() {
        val client = api ?: return
        run("Nexus Notify install failed", loud = true) {
            notifierBusy = true
            notifierDownloadedBytes = 0L
            notifierTotalBytes = 0L
            try {
                val context = getApplication<Application>()
                val version = client.appVersion()
                val expected = version.optJSONObject("notifier")?.optString("sha256", "").orEmpty()
                if (expected.isEmpty()) throw HiveException("Nexus Notify is not available on this HIVE")
                val directory = java.io.File(context.cacheDir, "updates").apply { mkdirs() }
                val destination = java.io.File(directory, "nexus-notify.apk")
                client.downloadNotifierApk(destination) { downloaded, total ->
                    notifierDownloadedBytes = downloaded
                    notifierTotalBytes = total
                }
                val got = withContext(Dispatchers.IO) { sha256hexFile(destination) }
                if (got != expected) {
                    destination.delete()
                    throw HiveException("Nexus Notify download corrupted — try again")
                }
                withContext(Dispatchers.IO) { verifyNotifierArchive(destination) }
                withContext(Dispatchers.IO) {
                    UpdateInstaller.commit(context, destination, NOTIFIER_PACKAGE)
                }
                status = "Nexus Notify installing…"
            } finally {
                notifierBusy = false
            }
        }
    }

    private fun installedApkHash(packageName: String): String? {
        val context = getApplication<Application>()
        val source = runCatching {
            context.packageManager.getApplicationInfo(packageName, 0).sourceDir
        }.getOrNull() ?: return null
        return sha256hexFile(java.io.File(source))
    }

    private fun verifyNotifierArchive(apk: java.io.File) {
        val context = getApplication<Application>()
        val packageManager = context.packageManager
        val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            PackageManager.GET_SIGNING_CERTIFICATES
        } else {
            @Suppress("DEPRECATION")
            PackageManager.GET_SIGNATURES
        }
        val archive = packageManager.getPackageArchiveInfo(apk.absolutePath, flags)
            ?: throw HiveException("Nexus Notify package is invalid")
        if (archive.packageName != NOTIFIER_PACKAGE) {
            throw HiveException("download is not Nexus Notify")
        }
        val connect = packageManager.getPackageInfo(context.packageName, flags)
        if (packageSigners(archive) != packageSigners(connect)) {
            throw HiveException("Nexus Notify signer does not match Nexus Connect")
        }
    }

    @Suppress("DEPRECATION")
    private fun packageSigners(info: android.content.pm.PackageInfo): Set<String> {
        val signatures = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            info.signingInfo?.apkContentsSigners
        } else {
            info.signatures
        }
        return signatures.orEmpty().map { sha256hex(it.toByteArray()) }.toSet()
            .takeIf { it.isNotEmpty() }
            ?: throw HiveException("APK has no signing certificate")
    }

    // ------------------------------------------------------------ sessions

    fun enroll(server: String, wantedHandle: String, inviteCode: String, password: String = "") {
        val srv = server.trim().removeSuffix("/")
        val h = wantedHandle.trim()
        if (srv.isEmpty() || h.isEmpty()) {
            errorDialog = "Server and handle are required"
            return
        }
        run("enroll failed", loud = true) {
            status = "Connecting…"
            val c = HiveClient(srv, acceptSelfSigned = BuildConfig.DEBUG)
            c.info()
            status = "Creating account…"
            val identity = Key.generate()
            val device = Key.generate()
            val (acct, _) = c.register(
                identity, device, h,
                android.os.Build.MODEL ?: "phone",
                inviteCode.trim().ifEmpty { null },
            )
            c.auth(acct, device)
            if (password.isNotBlank()) {
                status = "Securing recovery…"
                c.escrowSet("password", Escrow.seal(password, identity.seed))
            }
            enrollment = Enrollment(srv, c.pin!!, acct, h, identity, device)
            store.save(enrollment!!)
            store.sessionToken = c.token
            api = c
            enrolled = true
            handle = h
            accountId = acct
            status = ""
            afterSignIn(c)
        }
    }

    /** Handle + recovery password → decrypt identity seed → enroll this phone. */
    fun signInWithPassword(server: String, wantedHandle: String, password: String) {
        val srv = server.trim().removeSuffix("/")
        val h = wantedHandle.trim().removePrefix("@")
        if (srv.isEmpty() || h.isEmpty() || password.isEmpty()) {
            errorDialog = "Server, handle, and password are required"
            return
        }
        run("sign-in failed", loud = true) {
            status = "Connecting…"
            val c = HiveClient(srv, acceptSelfSigned = BuildConfig.DEBUG)
            c.info()
            status = "Fetching recovery bundle…"
            val (acct, blob) = c.escrowFetch(h, "password")
            status = "Unlocking…"
            val identity = Key(Escrow.open(password, blob))
            if (accountIdFor(identity.publicBytes) != acct) {
                throw HiveException("recovery data does not match the account")
            }
            val device = Key.generate()
            c.recoverDevice(acct, identity, device, android.os.Build.MODEL ?: "phone")
            c.auth(acct, device)
            enrollment = Enrollment(srv, c.pin!!, acct, h, identity, device)
            store.save(enrollment!!)
            store.sessionToken = c.token
            api = c
            enrolled = true
            handle = h
            accountId = acct
            status = ""
            afterSignIn(c)
        }
    }

    /** Join an existing account: show a code, an enrolled device approves. */
    fun linkStart(server: String) {
        val srv = server.trim().removeSuffix("/")
        if (srv.isEmpty()) {
            errorDialog = "Server is required"
            return
        }
        run("link failed", loud = true) {
            status = "Connecting…"
            val c = HiveClient(srv, acceptSelfSigned = BuildConfig.DEBUG)
            c.info()
            val device = Key.generate()
            val code = c.linkBegin(device, android.os.Build.MODEL ?: "phone")
            linkCode = code
            linkWaiting = true
            status = ""
            viewModelScope.launch {
                try {
                    while (isActive && linkWaiting) {
                        delay(2000)
                        val acct = try {
                            c.linkStatus(code)
                        } catch (e: HiveException) {
                            linkWaiting = false
                            linkCode = null
                            errorDialog = "Link request expired — start over"
                            return@launch
                        }
                        if (acct != null) {
                            c.auth(acct, device)
                            val h = c.whoami().optString("handle")
                            enrollment = Enrollment(srv, c.pin!!, acct, h, null, device)
                            store.save(enrollment!!)
                            store.sessionToken = c.token
                            api = c
                            enrolled = true
                            handle = h
                            accountId = acct
                            linkWaiting = false
                            linkCode = null
                            afterSignIn(c)
                            return@launch
                        }
                    }
                } catch (e: Exception) {
                    linkWaiting = false
                    linkCode = null
                    errorDialog = e.message ?: "linking failed"
                }
            }
        }
    }

    fun linkCancel() {
        linkWaiting = false
        linkCode = null
    }

    private fun signIn(e: Enrollment) {
        run("sign-in failed") {
            status = "Connecting…"
            var c = SessionManager.client(getApplication(), e, BuildConfig.DEBUG)
            api = c
            status = ""
            try {
                afterSignIn(c)
            } catch (_: SessionExpiredException) {
                c = SessionManager.client(
                    getApplication(), e, BuildConfig.DEBUG, invalidToken = c.token,
                )
                api = c
                afterSignIn(c)
            }
        }
    }

    private suspend fun afterSignIn(c: HiveClient) {
        enrollment?.let { CallSession.configure(getApplication(), c, it) }
        startupStep("legal review") { refreshLegalStatus(c) }
        enrollment?.let {
            startupStep("WIRE key publication") { c.wirePublish(it.device, store.wireKey()) }
            try {
                NexusNotifier.provision(getApplication(), it, c)
            } catch (error: Exception) {
                android.util.Log.w("NexusNotifier", "notification companion provisioning failed", error)
            }
        }
        startupStep("conversations") { refreshChatStates() }
        startupStep("direct messages") { refreshDirectMessages() }
        startupStep("message history restore") { consumeMessageHistoryOffers() }
        startupStep("feed") { refreshFeed() }
        startupStep("social graph") { refreshGraph() }
        startupStep("alerts") { refreshAlerts() }
        startupStep("profile") { refreshMyProfile() }
        startupStep("Folds") { refreshFolds() }
        startupStep("update check") { checkForUpdate(c) }
        ConnectNotifications.syncNow(getApplication())
        streamEventsJob?.cancel()
        streamEventsJob = viewModelScope.launch {
            HiveStreamEvents.frames.collect { frame ->
                if (frame.optString("type") == "connect_notif") {
                    runCatching { refreshAlerts(notifyNew = true) }
                    when (frame.optString("kind")) {
                        "post", "comment", "reaction", "mention" -> runCatching { refreshFeed() }
                        "follow_request", "follow_accepted", "follow" -> runCatching { refreshGraph() }
                    }
                }
                if (frame.optString("type") == "wire_msg") {
                    runCatching { refreshDirectMessages(notifyNew = true) }
                }
                if (frame.optString("type") == "wire_receipt") {
                    runCatching { refreshReceiptStates() }
                }
                if (frame.optString("type") == "wire_history_sync_request") {
                    val requestingDevice = frame.optString("requesting_device")
                    if (requestingDevice.isNotEmpty()) {
                        runCatching { publishMessageHistory(requestingDevice) }
                    }
                }
                if (frame.optString("type") == "wire_history_sync_offer") {
                    val target = frame.optString("target_device")
                    val current = enrollment?.let { deviceIdFor(it.device.publicBytes) }
                    if (target.isEmpty() || target == current) {
                        runCatching { consumeMessageHistoryOffers() }
                    }
                }
                if (frame.optString("type") in setOf("wire_request", "wire_accepted")) {
                    if (frame.optString("type") == "wire_request") {
                        val from = frame.optString("from")
                        if (from.isNotEmpty()) {
                            hiddenConversations = hiddenConversations - from
                            store.hiddenConversations = hiddenConversations
                        }
                    }
                    runCatching { refreshChatStates() }
                }
                if (frame.optString("type") == "call_signal") {
                    handleCallSignal(frame)
                }
            }
        }
        viewModelScope.launch {
            delay(750)
            runCatching { syncMessageHistoryNow(requestOthers = true) }
            delay(1_500)
            runCatching { consumeMessageHistoryOffers() }
        }
        store.loadPendingCall()?.let { handleCallSignal(it) }
        connectForegroundStream()
        startRefreshLoop(c)
    }

    private suspend fun startupStep(name: String, block: suspend () -> Unit) {
        runCatching { block() }.onFailure { error ->
            android.util.Log.w("ConnectStartup", "$name refresh failed", error)
        }
    }

    fun setForeground(active: Boolean) {
        foregroundActive = active
        CallSession.setVisible(active)
    }

    private fun connectForegroundStream() {
        val client = api ?: return
        enrollment?.let { CallSession.configure(getApplication(), client, it) }
        CallSession.setVisible(foregroundActive)
    }

    fun restorePendingCall(callId: String?) {
        val frame = store.loadPendingCall() ?: return
        if (!callId.isNullOrEmpty() && frame.optString("call_id") != callId) return
        viewModelScope.launch { handleCallSignal(frame) }
    }

    private fun startRefreshLoop(c: HiveClient) {
        refreshJob?.cancel()
        refreshJob = viewModelScope.launch {
            var ticks = 0
            while (isActive && api === c) {
                delay(60_000)
                ticks += 1
                runCatching { refreshChatStates() }
                runCatching { refreshFeed() }
                runCatching { refreshAlerts() }
                if (ticks % 5 == 0) runCatching { checkForUpdate(c) }
            }
        }
    }

    fun refreshOnResume() {
        val c = api ?: return
        viewModelScope.launch {
            runCatching { refreshChatStates() }
            runCatching { refreshDirectMessages() }
            runCatching { refreshFeed() }
            runCatching { refreshAlerts() }
            runCatching { refreshGraph() }
            runCatching { checkForUpdate(c) }
        }
    }

    // ------------------------------------------------------------ messages

    private suspend fun refreshChatStates() {
        val array = api?.wireConversations()?.optJSONArray("conversations") ?: return
        var states = (0 until array.length()).map { index ->
            val value = array.getJSONObject(index)
            ChatContact(Author.of(value), value.optString("direction"))
        }
        val incoming = states.filter { it.direction == "incoming" }
        if (autoAcceptMessageInvites && incoming.isNotEmpty()) {
            incoming.forEach { api?.wireRespond(it.author.accountId, true) }
            hiddenConversations = hiddenConversations - incoming.map { it.author.accountId }.toSet()
            store.hiddenConversations = hiddenConversations
            val refreshed = api?.wireConversations()?.optJSONArray("conversations") ?: array
            states = (0 until refreshed.length()).map { index ->
                val value = refreshed.getJSONObject(index)
                ChatContact(Author.of(value), value.optString("direction"))
            }
        } else {
            val fresh = store.newNotificationIds("invite", incoming.map { it.author.accountId })
            val failed = incoming.filter { it.author.accountId in fresh }.filterNot {
                ConnectNotifications.postMessageInvite(getApplication(), it.author.accountId, it.author.label)
            }
            store.retryNotificationIds("invite", failed.map { it.author.accountId })
        }
        chatStates = states
    }

    fun requestChat(contact: Author) = run("message request failed", loud = true) {
        api?.wireRequest(contact.accountId)
        hiddenConversations = hiddenConversations - contact.accountId
        store.hiddenConversations = hiddenConversations
        refreshChatStates()
    }

    fun respondToChat(contact: Author, accept: Boolean) =
        run("message request response failed", loud = true) {
            api?.wireRespond(contact.accountId, accept)
            if (accept) {
                hiddenConversations = hiddenConversations - contact.accountId
                store.hiddenConversations = hiddenConversations
            }
            refreshChatStates()
        }

    private suspend fun refreshDirectMessages(notifyNew: Boolean = false) {
        val c = api ?: return
        val array = c.wireInbox().optJSONArray("messages") ?: return
        val verified = verifyInbox(accountId, store.wireKey(), array)
        val knownMessages = (directMessages + verified.messages).associateBy { it.id }
        val validDeletions = verified.deletions.filter { deletion ->
            knownMessages[deletion.targetId]?.let { target ->
                deletion.senderAccount == if (target.mine) accountId else target.peerAccount
            } == true
        }
        val deletedIds = store.deletedMessageIds + validDeletions.map { it.targetId }
        store.deletedMessageIds = deletedIds
        directMessages = (directMessages + verified.messages)
            .associateBy { it.id }.values.sortedBy { it.sent }
            .filterNot { it.id in deletedIds }
        val activePeers = verified.messages.map { it.peerAccount }.filter { it.isNotEmpty() }.toSet()
        if (activePeers.isNotEmpty()) {
            hiddenConversations = hiddenConversations - activePeers
            store.hiddenConversations = hiddenConversations
        }
        store.saveDirectMessages(directMessages)
        store.queueMessageNotifications(verified.messages)
        if (dmOpen) {
            verified.messages.forEach { store.completeMessageAlert(it.id) }
        } else if (notifyNew) {
            ConnectNotifications.deliverPendingMessages(getApplication())
        } else if (verified.messages.isNotEmpty()) {
            ConnectNotifications.syncNow(getApplication(), expedited = true)
        }
        c.wireAck(verified.verifiedIds)
        if (verified.messages.isNotEmpty() || validDeletions.isNotEmpty()) {
            queueMessageHistoryPublish()
        }
        refreshReceiptStates()
    }

    private fun queueMessageHistoryPublish() {
        historySyncJob?.cancel()
        historySyncJob = viewModelScope.launch {
            delay(1_500)
            runCatching { publishMessageHistory() }
        }
    }

    fun syncMessageHistory() = run("message history sync failed", loud = true) {
        syncMessageHistoryNow(requestOthers = true)
    }

    private suspend fun syncMessageHistoryNow(requestOthers: Boolean) {
        messageHistorySyncing = true
        try {
            consumeMessageHistoryOffers()
            if (requestOthers) api?.wireHistorySyncRequest()
            publishMessageHistory()
            messageHistoryLastSync = store.lastMessageHistorySync
        } finally {
            messageHistorySyncing = false
        }
    }

    private suspend fun publishMessageHistory(targetDeviceId: String? = null) {
        val c = api ?: return
        val e = enrollment ?: return
        val directory = c.wireDirectory(accountId)
        val identityPub = directory.getString("identity_pub")
        val devices = directory.getJSONArray("devices")
        val currentDeviceId = deviceIdFor(e.device.publicBytes)
        val publisherDevice = (0 until devices.length())
            .map { devices.getJSONObject(it) }
            .firstOrNull { it.optString("device_id") == currentDeviceId }
            ?: return
        if (!WireCrypto.validateDevice(accountId, identityPub, publisherDevice)) {
            throw HiveException("current messaging credential failed verification")
        }
        val targets = (0 until devices.length())
            .map { devices.getJSONObject(it) }
            .filter { it.optString("device_id") != currentDeviceId }
            .filter { targetDeviceId == null || it.optString("device_id") == targetDeviceId }
        messageHistoryDeviceCount = targets.size + 1
        if (targets.isEmpty()) {
            store.lastMessageHistorySync = System.currentTimeMillis() / 1000
            messageHistoryLastSync = store.lastMessageHistorySync
            return
        }
        val snapshot = store.exportMessageHistory()
        val syncedThrough = directMessages.maxOfOrNull { it.sent } ?: 0L
        for (target in targets) {
            if (!WireCrypto.validateDevice(accountId, identityPub, target)) {
                continue
            }
            val snapshotId = ByteArray(16).also(SecureRandom()::nextBytes)
                .joinToString("") { "%02x".format(it) }
            val targetId = target.getString("device_id")
            val encryptedSnapshot = WireCrypto.encrypt(
                unb64(target.getString("wire_pub")),
                "history-blob:$snapshotId",
                snapshot,
            )
            val snapshotHash = sha256hex(encryptedSnapshot.toByteArray())
            val signed = WireCrypto.signedHistorySync(
                snapshotId,
                accountId,
                currentDeviceId,
                targetId,
                snapshotHash,
                syncedThrough,
            )
            val pointer = JSONObject().apply {
                put("v", 1)
                put("kind", "history_sync")
                put("id", snapshotId)
                put("sender_account", accountId)
                put("sender_identity_pub", identityPub)
                put("sender_device", publisherDevice)
                put("target_device", targetId)
                put("snapshot_hash", snapshotHash)
                put("synced_through", syncedThrough)
                put("signature", b64(e.device.sign(signed.toByteArray())))
            }.toString().toByteArray()
            c.wireHistorySyncPublish(
                snapshotId,
                targetId,
                snapshotHash,
                encryptedSnapshot,
                WireCrypto.encrypt(
                    unb64(target.getString("wire_pub")),
                    "history-pointer:$snapshotId",
                    pointer,
                ),
                syncedThrough,
            )
        }
        store.lastMessageHistorySync = System.currentTimeMillis() / 1000
        messageHistoryLastSync = store.lastMessageHistorySync
    }

    private suspend fun consumeMessageHistoryOffers() {
        val c = api ?: return
        val e = enrollment ?: return
        val currentDeviceId = deviceIdFor(e.device.publicBytes)
        val offers = c.wireHistorySyncOffers().optJSONArray("offers") ?: return
        for (index in 0 until offers.length()) {
            val offer = offers.getJSONObject(index)
            val snapshotId = offer.optString("snapshot_id")
            val payload = runCatching {
                JSONObject(
                    String(
                        WireCrypto.decrypt(
                            store.wireKey(),
                            "history-pointer:$snapshotId",
                            offer.getString("envelope"),
                        ),
                    ),
                )
            }.getOrNull() ?: continue
            val publisherDevice = payload.optJSONObject("sender_device") ?: continue
            val senderAccount = payload.optString("sender_account")
            val snapshotHash = payload.optString("snapshot_hash")
            val syncedThrough = payload.optLong("synced_through")
            if (payload.optString("kind") != "history_sync"
                || payload.optString("id") != snapshotId
                || payload.optString("target_device") != currentDeviceId
                || publisherDevice.optString("device_id") != offer.optString("publisher")
                || snapshotHash != offer.optString("snapshot_hash")
                || sha256hex(offer.optString("snapshot").toByteArray()) != snapshotHash
            ) continue
            val identityPub = payload.optString("sender_identity_pub")
            if (senderAccount != accountId
                || !WireCrypto.validateDevice(senderAccount, identityPub, publisherDevice)
            ) continue
            val signed = WireCrypto.signedHistorySync(
                snapshotId,
                senderAccount,
                publisherDevice.getString("device_id"),
                currentDeviceId,
                snapshotHash,
                syncedThrough,
            )
            if (!Key.verify(
                    unb64(publisherDevice.getString("device_pub")),
                    signed.toByteArray(),
                    unb64(payload.getString("signature")),
                )
            ) continue
            val snapshot = WireCrypto.decrypt(
                store.wireKey(),
                "history-blob:$snapshotId",
                offer.getString("snapshot"),
            )
            val (merged, mergedThrough) = store.mergeMessageHistory(snapshot)
            directMessages = merged
            hiddenConversations = store.hiddenConversations
            refreshChatStates()
            c.wireHistorySyncConsume(snapshotId, maxOf(syncedThrough, mergedThrough))
        }
        messageHistoryLastSync = store.lastMessageHistorySync
    }

    private suspend fun refreshReceiptStates() {
        val c = api ?: return
        val outgoing = directMessages.filter { it.mine }.map { it.id }
        if (outgoing.isEmpty()) return
        val statuses = mutableMapOf<String, String>()
        outgoing.chunked(500).forEach { ids ->
            val rows = c.wireReceipts(ids).optJSONArray("receipts") ?: return@forEach
            for (index in 0 until rows.length()) {
                val row = rows.getJSONObject(index)
                statuses[row.getString("msg_id")] = when {
                    !row.isNull("read_at") -> "read"
                    !row.isNull("received_at") -> "received"
                    !row.isNull("delivered_at") -> "delivered"
                    else -> "sent"
                }
            }
        }
        if (statuses.isNotEmpty()) {
            directMessages = directMessages.map { message ->
                statuses[message.id]?.let { message.copy(status = it) } ?: message
            }
            store.saveDirectMessages(directMessages)
        }
    }

    fun openConversation(accountId: String) {
        hiddenConversations = hiddenConversations - accountId
        store.hiddenConversations = hiddenConversations
        val unread = directMessages.filter { !it.mine && it.peerAccount == accountId }.map { it.id }
        if (unread.isNotEmpty()) viewModelScope.launch {
            runCatching { api?.wireRead(unread) }
        }
    }

    fun deleteMessage(message: DirectMessage, forEveryone: Boolean) {
        if (forEveryone && !message.mine) return
        run("message delete failed", loud = forEveryone) {
            if (forEveryone) sendDeletion(message)
            store.deletedMessageIds = store.deletedMessageIds + message.id
            directMessages = directMessages.filterNot { it.id == message.id }
            store.saveDirectMessages(directMessages)
            queueMessageHistoryPublish()
        }
    }

    fun deleteConversation(accountId: String, handle: String) {
        deleteConversations(mapOf(accountId to handle))
    }

    fun deleteConversations(conversations: Map<String, String>) {
        if (conversations.isEmpty()) return
        val removed = directMessages.filter { message ->
            conversations.any { (accountId, handle) ->
                message.peerAccount == accountId ||
                    (message.peerAccount.isEmpty() && message.peerHandle.equals(handle, ignoreCase = true))
            }
        }.map { it.id }.toSet()
        store.deletedMessageIds = store.deletedMessageIds + removed
        directMessages = directMessages.filterNot { message ->
            conversations.any { (accountId, handle) ->
                message.peerAccount == accountId ||
                    (message.peerAccount.isEmpty() && message.peerHandle.equals(handle, ignoreCase = true))
            }
        }
        hiddenConversations = hiddenConversations + conversations.keys
        store.hiddenConversations = hiddenConversations
        store.saveDirectMessages(directMessages)
        queueMessageHistoryPublish()
    }

    fun refreshMessages() = run("message refresh failed") {
        refreshChatStates()
        refreshDirectMessages()
        syncMessageHistoryNow(requestOthers = true)
    }

    private suspend fun handleCallSignal(frame: JSONObject) {
        CallSession.receive(frame)
    }

    private fun callManager(): SecureCallManager = CallSession.manager()

    fun startCall(contact: Author, kind: String) {
        viewModelScope.launch {
            val relayServers = runCatching { api?.callIceServers().orEmpty() }.getOrDefault(emptyList())
            callManager().setRelayServers(relayServers)
            runCatching { CallService.start(getApplication(), contact, kind) }
                .onFailure { errorDialog = "Could not start the call service. Check microphone permission." }
        }
    }
    fun acceptCall() {
        val kind = calls?.state?.kind ?: return
        runCatching { CallService.accept(getApplication(), kind) }
            .onFailure { errorDialog = "Could not start the call service. Check microphone permission." }
    }
    fun rejectCall() = calls?.reject()
    fun hangupCall() = calls?.hangup()
    fun toggleCallMute() = calls?.toggleMute()
    fun toggleCallSpeaker() = calls?.toggleSpeaker()
    fun toggleCallCamera() = calls?.toggleCamera()
    fun switchCallCamera() = calls?.switchCamera()

    fun sendDirect(
        target: String,
        body: String,
        attachmentUri: Uri? = null,
        attachmentMime: String? = null,
    ) {
        val cleanTarget = target.trim().removePrefix("@")
        val cleanBody = body.trim()
        if (cleanTarget.isEmpty() || (cleanBody.isEmpty() && attachmentUri == null)) return
        run("message send failed", loud = true) {
            val c = api ?: return@run
            val e = enrollment ?: throw HiveException("device enrollment unavailable")
            val targetDirectory = c.wireDirectory(cleanTarget)
            val ownDirectory = c.wireDirectory(accountId)
            val ownDevices = ownDirectory.getJSONArray("devices")
            val targetDevices = targetDirectory.getJSONArray("devices")
            if (targetDevices.length() == 0) {
                throw HiveException("@$cleanTarget has not enabled encrypted messaging yet")
            }
            val senderDeviceId = deviceIdFor(e.device.publicBytes)
            val senderDevice = (0 until ownDevices.length())
                .map { ownDevices.getJSONObject(it) }
                .firstOrNull { it.optString("device_id") == senderDeviceId }
                ?: throw HiveException("current messaging device is not in the signed directory")
            val ownIdentityPub = ownDirectory.getString("identity_pub")
            if (!WireCrypto.validateDevice(accountId, ownIdentityPub, senderDevice)) {
                throw HiveException("current messaging credential failed verification")
            }
            val id = ByteArray(16).also(SecureRandom()::nextBytes)
                .joinToString("") { "%02x".format(it) }
            val sent = System.currentTimeMillis() / 1000
            val recipientAccount = targetDirectory.getString("account_id")
            val recipientHandle = targetDirectory.getString("handle")
            val attachment = attachmentUri?.let { uri ->
                val mime = attachmentMime?.takeIf {
                    it.startsWith("image/") || it.startsWith("audio/") || it.startsWith("video/")
                } ?: throw HiveException("unsupported message attachment")
                val label = when {
                    mime.startsWith("audio/") -> "voice note"
                    mime.startsWith("video/") -> "video"
                    else -> "photo"
                }
                status = "Encrypting $label…"
                val bytes = if (mime.startsWith("image/")) processImage(uri)
                    else processMessageMedia(uri)
                val encrypted = WireCrypto.encryptAttachment(id, bytes)
                status = "Uploading encrypted $label…"
                val blobId = c.blobUpload(
                    encrypted.ciphertext,
                    public = false,
                    purpose = "connect_media",
                )
                MessageAttachment(
                    blobId,
                    encrypted.key,
                    encrypted.nonce,
                    if (mime.startsWith("image/")) "image/jpeg" else mime,
                )
            }
            val signed = attachment?.let {
                WireCrypto.signedMessageV2(id, accountId, recipientAccount, sent, cleanBody, it)
            } ?: WireCrypto.signedMessage(id, accountId, recipientAccount, sent, cleanBody)
            val payload = JSONObject().apply {
                put("v", if (attachment == null) 1 else 2)
                put("kind", "message")
                put("id", id)
                put("sender_account", accountId)
                put("sender_handle", handle)
                put("sender_identity_pub", ownIdentityPub)
                put("sender_device", senderDevice)
                put("recipient_account", recipientAccount)
                put("recipient_handle", recipientHandle)
                put("sent", sent)
                put("body", cleanBody)
                attachment?.let {
                    put("attachment", JSONObject().apply {
                        put("blob_id", it.blobId)
                        put("key", it.key)
                        put("nonce", it.nonce)
                        put("mime", it.mime)
                    })
                }
                put("signature", b64(e.device.sign(signed.toByteArray())))
            }.toString().toByteArray()
            val recipients = buildList {
                for (i in 0 until targetDevices.length()) add(targetDevices.getJSONObject(i))
                for (i in 0 until ownDevices.length()) add(ownDevices.getJSONObject(i))
            }.distinctBy { it.optString("device_id") }
            recipients.forEach { device ->
                val directory = if (device.optString("device_id") == senderDeviceId ||
                    (0 until ownDevices.length()).any {
                        ownDevices.getJSONObject(it).optString("device_id") == device.optString("device_id")
                    }
                ) ownDirectory else targetDirectory
                if (!WireCrypto.validateDevice(
                        directory.getString("account_id"), directory.getString("identity_pub"), device,
                    )
                ) throw HiveException("recipient messaging credential failed verification")
                c.wireSend(
                    device.getString("device_id"),
                    id,
                    WireCrypto.encrypt(unb64(device.getString("wire_pub")), id, payload),
                    attachment?.blobId,
                )
            }
            status = ""
            refreshDirectMessages()
        }
    }

    fun loadMessageAttachment(message: DirectMessage) {
        val attachment = message.attachment ?: return
        if (message.id in messageImages || message.id in messageMediaFiles) return
        viewModelScope.launch {
            runCatching {
                val ciphertext = api?.wireAttachmentFetch(attachment.blobId)
                    ?: throw HiveException("messaging is unavailable")
                WireCrypto.decryptAttachment(message.id, attachment, ciphertext)
            }.onSuccess { bytes ->
                if (attachment.mime.startsWith("image/")) {
                    messageImages = messageImages + (message.id to bytes)
                } else {
                    val extension = when {
                        attachment.mime.startsWith("audio/") -> ".m4a"
                        attachment.mime == "video/quicktime" -> ".mov"
                        else -> ".mp4"
                    }
                    val file = File(messageMediaDirectory, "${message.id}$extension")
                    FileOutputStream(file).use { it.write(bytes) }
                    messageMediaFiles = messageMediaFiles + (message.id to file)
                }
            }
        }
    }

    private suspend fun processMessageMedia(uri: Uri): ByteArray = withContext(Dispatchers.IO) {
        val resolver = getApplication<Application>().contentResolver
        val output = ByteArrayOutputStream()
        val buffer = ByteArray(64 * 1_024)
        resolver.openInputStream(uri)?.use { input ->
            var total = 0
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                total += count
                if (total > MESSAGE_MEDIA_MAX_BYTES) {
                    throw HiveException("voice and video messages must be 25 MB or smaller")
                }
                output.write(buffer, 0, count)
            }
        } ?: throw HiveException("couldn't read message media")
        output.toByteArray()
    }

    private suspend fun sendDeletion(message: DirectMessage) {
        val c = api ?: throw HiveException("messaging is unavailable")
        val e = enrollment ?: throw HiveException("device enrollment unavailable")
        val targetDirectory = c.wireDirectory(message.peerAccount.ifEmpty { message.peerHandle })
        val ownDirectory = c.wireDirectory(accountId)
        val ownDevices = ownDirectory.getJSONArray("devices")
        val targetDevices = targetDirectory.getJSONArray("devices")
        val senderDeviceId = deviceIdFor(e.device.publicBytes)
        val senderDevice = (0 until ownDevices.length()).map { ownDevices.getJSONObject(it) }
            .firstOrNull { it.optString("device_id") == senderDeviceId }
            ?: throw HiveException("current messaging device is not in the signed directory")
        val ownIdentityPub = ownDirectory.getString("identity_pub")
        val eventId = ByteArray(16).also(SecureRandom()::nextBytes)
            .joinToString("") { "%02x".format(it) }
        val deletedAt = System.currentTimeMillis() / 1000
        val recipientAccount = targetDirectory.getString("account_id")
        val signed = WireCrypto.signedDeletion(
            eventId, message.id, accountId, recipientAccount, deletedAt,
        )
        val payload = JSONObject().apply {
            put("v", 1)
            put("kind", "delete")
            put("id", eventId)
            put("target_id", message.id)
            put("sender_account", accountId)
            put("sender_handle", handle)
            put("sender_identity_pub", ownIdentityPub)
            put("sender_device", senderDevice)
            put("recipient_account", recipientAccount)
            put("recipient_handle", targetDirectory.getString("handle"))
            put("deleted_at", deletedAt)
            put("signature", b64(e.device.sign(signed.toByteArray())))
        }.toString().toByteArray()
        c.wireRetract(message.id)
        val ownDeviceIds = (0 until ownDevices.length())
            .map { ownDevices.getJSONObject(it).optString("device_id") }.toSet()
        val recipients = buildList {
            for (index in 0 until targetDevices.length()) add(targetDevices.getJSONObject(index))
            for (index in 0 until ownDevices.length()) add(ownDevices.getJSONObject(index))
        }.distinctBy { it.optString("device_id") }
        recipients.forEach { device ->
            val directory = if (device.optString("device_id") in ownDeviceIds) {
                ownDirectory
            } else {
                targetDirectory
            }
            if (!WireCrypto.validateDevice(
                    directory.getString("account_id"), directory.getString("identity_pub"), device,
                )
            ) throw HiveException("recipient messaging credential failed verification")
            c.wireSend(
                device.getString("device_id"), eventId,
                WireCrypto.encrypt(unb64(device.getString("wire_pub")), eventId, payload),
            )
        }
    }

    fun signOut() {
        CallSession.clear()
        val signedInApi = api
        if (signedInApi != null) {
            viewModelScope.launch {
                runCatching { signedInApi.revokeNotificationRelay() }
                runCatching { signedInApi.logout() }
            }
        }
        NexusNotifier.clear(getApplication())
        setForeground(false)
        streamEventsJob?.cancel()
        streamEventsJob = null
        refreshJob?.cancel()
        refreshJob = null
        updateAvailable = false
        updateVersion = ""
        updateSeverity = "normal"
        updateTitle = "Update available"
        updateNotes = listOf()
        api = null
        store.wipe()
        enrolled = false
        handle = ""
        accountId = ""
        posts = listOf()
        alerts = listOf()
        myProfile = null
        myPosts = listOf()
        viewedProfile = null
        detailPost = null
        folds = listOf()
        invites = listOf()
        status = ""
    }

    // ------------------------------------------------- account management

    /** Blocked users + device list for the settings sheet. */
    fun refreshAccountSettings() {
        val c = api ?: return
        run("loading account settings") {
            val b = c.blockedList().optJSONArray("blocked")
            blockedUsers = (0 until (b?.length() ?: 0)).map { Author.of(b!!.getJSONObject(it)) }
            val d = c.devices().optJSONArray("devices")
            deviceRows = (0 until (d?.length() ?: 0)).map { i ->
                val o = d!!.getJSONObject(i)
                DeviceRow(
                    id = o.getString("id"),
                    name = o.optString("name", "device"),
                    lastSeen = o.optLong("last_seen"),
                    revoked = o.optInt("revoked", 0) != 0 || o.optBoolean("revoked", false),
                )
            }
        }
    }

    fun unblockUser(a: Author) {
        val c = api ?: return
        run("unblock failed") {
            c.unblock(a.accountId)
            blockedUsers = blockedUsers.filter { it.accountId != a.accountId }
        }
    }

    fun revokeDevice(d: DeviceRow) {
        val c = api ?: return
        run("revoke failed", loud = true) {
            c.deviceRevoke(d.id)
            deviceRows = deviceRows.map { if (it.id == d.id) it.copy(revoked = true) else it }
        }
    }

    /** Re-seal the identity key under a new recovery password. Only works on
     * a device that holds the identity key (account creator or password
     * sign-in). */
    fun changePassword(newPassword: String, onDone: () -> Unit) {
        val c = api ?: return
        val identity = store.load()?.identity
        if (identity == null) {
            errorDialog = "This phone doesn't hold the account key — change the password on the device that created the account."
            return
        }
        if (newPassword.length < 8) {
            errorDialog = "Password must be at least 8 characters"
            return
        }
        run("password change failed", loud = true) {
            c.escrowSet("password", Escrow.seal(newPassword, identity.seed))
            status = "Recovery password updated"
            onDone()
        }
    }

    /** GDPR data export → a JSON file in the phone's Downloads. */
    fun exportMyData() {
        val c = api ?: return
        run("export failed", loud = true) {
            val data = c.exportData().toString(2)
            val name = "connect-export-" + handle + "-" +
                java.text.SimpleDateFormat("yyyyMMdd-HHmmss", java.util.Locale.US)
                    .format(java.util.Date()) + ".json"
            withContext(Dispatchers.IO) {
                val application = getApplication<Application>()
                if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.Q) {
                    val resolver = application.contentResolver
                    val values = android.content.ContentValues().apply {
                        put(android.provider.MediaStore.Downloads.DISPLAY_NAME, name)
                        put(android.provider.MediaStore.Downloads.MIME_TYPE, "application/json")
                    }
                    val uri = resolver.insert(
                        android.provider.MediaStore.Downloads.EXTERNAL_CONTENT_URI, values,
                    ) ?: throw HiveException("could not create the export file")
                    resolver.openOutputStream(uri)?.use { it.write(data.toByteArray()) }
                        ?: throw HiveException("could not write the export file")
                } else {
                    val directory = application.getExternalFilesDir(
                        android.os.Environment.DIRECTORY_DOWNLOADS,
                    ) ?: throw HiveException("Downloads storage is unavailable")
                    java.io.File(directory, name).writeText(data)
                }
            }
            status = "Saved to Downloads: $name"
        }
    }

    /** Permanently delete the account server-side, then wipe this phone.
     * Requires re-typing the handle and proving the recovery password to
     * guard against accidental (or someone-else's-thumb) deletions. */
    fun deleteAccount(typedHandle: String, password: String) {
        val c = api ?: return
        if (typedHandle.trim().removePrefix("@") != handle) {
            errorDialog = "Handle doesn't match this account"
            return
        }
        run("account deletion failed", loud = true) {
            // Prove the password by decrypting the escrowed identity key
            // locally — the same check as password sign-in.
            val hasEscrow = try {
                val (acct, blob) = c.escrowFetch(handle, "password")
                val identity = Key(Escrow.open(password, blob)) // throws on wrong password
                if (accountIdFor(identity.publicBytes) != acct) {
                    throw HiveException("recovery data does not match the account")
                }
                true
            } catch (e: HiveException) {
                if (e.message?.contains("no recovery set up") == true) false else throw e
            }
            if (!hasEscrow && password.isNotEmpty()) {
                throw HiveException("this account has no recovery password — leave the password blank and retype your handle to confirm")
            }
            c.accountDelete()
            signOut()
        }
    }

    // ---------------------------------------------------------------- feed

    private fun decryptFoldPayload(audience: String, envelope: String): JSONObject? {
        val circleId = audience.removePrefix("circle:")
        if (circleId == audience) return null
        val epoch = runCatching { JSONObject(envelope).getLong("epoch") }.getOrNull() ?: return null
        val key = store.foldKey(circleId, epoch) ?: return null
        return runCatching {
            JSONObject(String(WireCrypto.decryptFoldContent(key, circleId, envelope)))
        }.getOrNull()
    }

    private fun parsePost(p: JSONObject): Post {
        val mediaArr = p.optJSONArray("media")
        val audience = p.optString("audience")
        val folded = decryptFoldPayload(audience, p.optString("body"))
        return Post(
            postId = p.getString("post_id"),
            author = Author.of(p.getJSONObject("author")),
            created = p.optLong("created"),
            kind = p.optString("kind", "text"),
            body = if (audience.startsWith("circle:")) {
                folded?.optString("body") ?: "Encrypted Fold post unavailable on this device"
            } else p.optString("body"),
            media = if (mediaArr == null) listOf()
                    else (0 until mediaArr.length()).map { mediaArr.getString(it) },
            mediaTypes = p.optJSONArray("media_types")?.let { array ->
                (0 until array.length()).map { array.optString(it, "image/jpeg") }
            } ?: List(mediaArr?.length() ?: 0) { "image/jpeg" },
            altText = folded?.optString("alt_text") ?: p.optString("alt_text"),
            contentWarning = folded?.optString("content_warning") ?: p.optString("content_warning"),
            audience = audience,
            reactions = p.optLong("reactions"),
            myReaction = p.strOrNull("my_reaction"),
            comments = p.optLong("comments"),
            edited = if (p.isNull("edited")) null else p.optLong("edited"),
            pinned = p.optBoolean("pinned"),
            saved = p.optBoolean("saved"),
            encryptedBody = p.optString("body").takeIf { audience.startsWith("circle:") },
        )
    }

    private fun parsePosts(v: JSONObject): List<Post> {
        val arr = v.optJSONArray("posts") ?: return listOf()
        return (0 until arr.length()).map { i -> parsePost(arr.getJSONObject(i)) }
    }

    private suspend fun refreshFeed() {
        val c = api ?: return
        val v = when (feedMode) {
            "saved" -> c.savedPosts()
            "search" -> c.postSearch(feedQuery)
            else -> c.feed(null, 30)
        }
        posts = parsePosts(v)
        nextCursor = if (feedMode == "home") v.strOrNull("next_cursor") else null
    }

    fun showHomeFeed() = run("feed failed") {
        feedMode = "home"
        feedQuery = ""
        refreshFeed()
    }

    fun showSavedPosts() = run("saved posts failed") {
        feedMode = "saved"
        feedQuery = ""
        refreshFeed()
    }

    fun searchPosts(query: String) {
        if (query.isBlank()) return
        run("post search failed") {
            feedMode = "search"
            feedQuery = query.trim()
            refreshFeed()
        }
    }

    fun loadPostDraft(): PostDraft = store.loadPostDraft()

    fun savePostDraft(draft: PostDraft) = store.savePostDraft(draft)

    fun clearPostDraft() = store.clearPostDraft()

    fun loadMore() {
        if (feedMode != "home") return
        val cursor = nextCursor ?: return
        run("load failed") {
            val c = api ?: return@run
            val v = c.feed(cursor, 30)
            val more = parsePosts(v)
            if (more.isEmpty()) {
                nextCursor = null
            } else {
                posts = posts + more
                nextCursor = v.strOrNull("next_cursor")
            }
        }
    }

    fun refreshFoldFeed(fold: Fold) = run("Fold feed failed", loud = true) {
        if (!foldCanPost(fold)) return@run
        val value = api?.foldFeed(fold.circleId, null, 30) ?: return@run
        foldPosts = parsePosts(value)
        foldNextCursor = value.strOrNull("next_cursor")
    }

    fun foldCanPost(fold: Fold): Boolean = encryptedFoldsReady &&
        fold.keyEpoch > 0 && store.foldKey(fold.circleId, fold.keyEpoch) != null

    fun loadMoreFoldPosts(fold: Fold) {
        val cursor = foldNextCursor ?: return
        run("Fold feed failed") {
            val value = api?.foldFeed(fold.circleId, cursor, 30) ?: return@run
            val more = parsePosts(value)
            foldPosts = foldPosts + more
            foldNextCursor = value.strOrNull("next_cursor")
        }
    }

    fun createFoldPost(
        fold: Fold,
        body: String,
        attachments: List<PendingPostMedia>,
        altText: String,
        contentWarning: String,
        onDone: () -> Unit,
    ) {
        if (body.isBlank() && attachments.isEmpty()) return
        if (fold.keyEpoch <= 0) {
            errorDialog = "Encrypted Fold activity requires the pending HIVE update"
            return
        }
        run("Fold post failed", loud = true) {
            val c = api ?: return@run
            val key = store.foldKey(fold.circleId, fold.keyEpoch)
                ?: throw HiveException("Fold key is unavailable on this device")
            val media = attachments.mapIndexed { index, attachment ->
                status = "Encrypting Fold media ${index + 1}/${attachments.size}…"
                val bytes = if (attachment.mime.startsWith("video/")) {
                    processMessageMedia(attachment.uri)
                } else {
                    processImage(attachment.uri)
                }
                val encrypted = WireCrypto.encryptFoldContent(
                    key,
                    fold.circleId,
                    fold.keyEpoch,
                    bytes,
                )
                c.blobUpload(
                    encrypted.toByteArray(),
                    public = false,
                    purpose = "connect_media",
                )
            }
            val mediaTypes = attachments.map {
                if (it.mime.startsWith("video/")) it.mime else "image/jpeg"
            }
            val plaintext = JSONObject().apply {
                put("body", body.trim())
                put("alt_text", altText.trim())
                put("content_warning", contentWarning.trim())
            }.toString().toByteArray()
            val envelope = WireCrypto.encryptFoldContent(
                key,
                fold.circleId,
                fold.keyEpoch,
                plaintext,
            )
            c.postCreate(
                kind = when {
                    media.isEmpty() -> "text"
                    mediaTypes.any { it.startsWith("video/") } -> "video"
                    else -> "photo"
                },
                body = envelope,
                audience = "circle:${fold.circleId}",
                media = media,
                mediaTypes = mediaTypes,
                foldEpoch = fold.keyEpoch,
            )
            status = ""
            refreshFoldFeed(fold)
            onDone()
        }
    }

    fun refresh() = run("refresh failed") {
        refreshFeed()
        refreshGraph()
        refreshAlerts()
    }

    /** Downscale (≤2048px), re-encode JPEG — EXIF/location metadata never
     * leaves the phone (re-encoding drops it all). */
    private suspend fun processImage(uri: Uri): ByteArray = withContext(Dispatchers.IO) {
        val resolver = getApplication<Application>().contentResolver
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        resolver.openInputStream(uri)!!.use { BitmapFactory.decodeStream(it, null, bounds) }
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= 2048) sample *= 2
        val opts = BitmapFactory.Options().apply { inSampleSize = sample }
        val bmp = resolver.openInputStream(uri)!!.use {
            BitmapFactory.decodeStream(it, null, opts)
        } ?: throw HiveException("couldn't read image")
        val scale = 2048f / maxOf(bmp.width, bmp.height)
        val scaled = if (scale < 1f) {
            Bitmap.createScaledBitmap(
                bmp, (bmp.width * scale).toInt(), (bmp.height * scale).toInt(), true,
            )
        } else bmp
        val out = ByteArrayOutputStream()
        scaled.compress(Bitmap.CompressFormat.JPEG, 85, out)
        out.toByteArray()
    }

    fun createPost(
        body: String,
        audience: String,
        attachments: List<PendingPostMedia>,
        altText: String,
        contentWarning: String,
        onDone: () -> Unit,
    ) {
        if (body.isBlank() && attachments.isEmpty()) return
        run("post failed") {
            val c = api ?: return@run
            val media = attachments.mapIndexed { index, attachment ->
                val label = if (attachment.mime.startsWith("video/")) "video" else "photo"
                status = "Uploading $label ${index + 1}/${attachments.size}…"
                val bytes = if (attachment.mime.startsWith("video/")) {
                    processMessageMedia(attachment.uri)
                } else {
                    processImage(attachment.uri)
                }
                c.blobUpload(
                    bytes,
                    public = audience == "public",
                    purpose = "connect_media",
                )
            }
            val mediaTypes = attachments.map {
                if (it.mime.startsWith("video/")) it.mime else "image/jpeg"
            }
            status = ""
            c.postCreate(
                kind = when {
                    media.isEmpty() -> "text"
                    mediaTypes.any { it.startsWith("video/") } -> "video"
                    else -> "photo"
                },
                body = body.trim(),
                audience = audience,
                media = media,
                mediaTypes = mediaTypes,
                altText = altText.trim(),
                contentWarning = contentWarning.trim(),
            )
            feedMode = "home"
            refreshFeed()
            refreshMyPosts()
            onDone()
        }
    }

    fun deletePost(postId: String) = run("delete failed") {
        api?.postDelete(postId)
        foldPosts.firstOrNull { it.postId == postId }?.audience?.removePrefix("circle:")?.let { id ->
            folds.firstOrNull { it.circleId == id }?.let { refreshFoldFeed(it) }
        }
        refreshFeed()
        refreshMyPosts()
        if (viewedProfile != null) reloadViewedPosts()
    }

    /** Right to correction: edit your own post's text in place. */
    fun editPost(postId: String, body: String) = run("edit failed") {
        api?.postEdit(postId, body.trim())
        refreshFeed()
        refreshMyPosts()
        if (viewedProfile != null) reloadViewedPosts()
    }

    /** Right to correction: edit your own comment. */
    fun editComment(c: Comment, body: String) = run("edit failed") {
        api?.commentEdit(c.commentId, body.trim())
        openPostId?.let { loadComments(it) }
    }

    fun deleteComment(c: Comment) = run("delete failed") {
        api?.commentDelete(c.commentId)
        openPostId?.let { loadComments(it) }
    }

    /** kind = null clears; re-picking your current reaction clears too.
     * Optimistic: the count and glyph flip immediately, server refresh after. */
    fun setReaction(p: Post, kind: String?) {
        val clearing = kind == null || kind == p.myReaction
        val updated = p.copy(
            myReaction = if (clearing) null else kind,
            reactions = when {
                clearing -> (p.reactions - 1).coerceAtLeast(0)
                p.myReaction == null -> p.reactions + 1
                else -> p.reactions // changed kind, count unchanged
            },
        )
        fun swap(list: List<Post>) = list.map { if (it.postId == p.postId) updated else it }
        posts = swap(posts)
        foldPosts = swap(foldPosts)
        viewedPosts = swap(viewedPosts)
        myPosts = swap(myPosts)
        if (detailPost?.postId == p.postId) detailPost = updated
        run("reaction failed") {
            if (clearing) api?.unreact(p.postId) else api?.react(p.postId, kind!!)
            if (p.audience.startsWith("circle:")) {
                folds.firstOrNull { "circle:${it.circleId}" == p.audience }?.let { refreshFoldFeed(it) }
            } else {
                refreshFeed()
            }
            if (viewedProfile != null) reloadViewedPosts()
        }
    }

    fun toggleSavePost(p: Post) = run("save failed") {
        if (p.saved) api?.postUnsave(p.postId) else api?.postSave(p.postId)
        val updated = p.copy(saved = !p.saved)
        fun swap(list: List<Post>) = list.map { if (it.postId == p.postId) updated else it }
        posts = if (feedMode == "saved" && p.saved) posts.filterNot { it.postId == p.postId }
            else swap(posts)
        viewedPosts = swap(viewedPosts)
        myPosts = swap(myPosts)
        if (detailPost?.postId == p.postId) detailPost = updated
    }

    fun togglePinPost(p: Post) = run("pin failed") {
        api?.postPin(p.postId, !p.pinned)
        refreshFeed()
        refreshMyPosts()
        if (viewedProfile != null) reloadViewedPosts()
    }

    fun loadPostRevisions(postId: String) = run("history failed") {
        val value = api?.postRevisions(postId) ?: return@run
        val array = value.optJSONArray("revisions")
        postRevisions = (0 until (array?.length() ?: 0)).map { index ->
            val revision = array!!.getJSONObject(index)
            PostRevision(revision.optString("body"), revision.optLong("replaced_at"))
        }
        revisionsPostId = postId
    }

    fun closePostRevisions() {
        revisionsPostId = null
        postRevisions = emptyList()
    }

    fun loadPostVideo(blobId: String) {
        if (blobId in postVideoFiles) return
        viewModelScope.launch {
            runCatching {
                val bytes = api?.mediaFetch(blobId) ?: throw HiveException("media unavailable")
                if (bytes.size > MESSAGE_MEDIA_MAX_BYTES) throw HiveException("video is too large to play")
                File(postMediaDirectory, "$blobId.mp4").also { file ->
                    FileOutputStream(file).use { it.write(bytes) }
                }
            }.onSuccess { file -> postVideoFiles = postVideoFiles + (blobId to file) }
        }
    }

    fun loadFoldMedia(post: Post, blobId: String, mime: String) {
        if (blobId in foldMediaFiles) return
        val circleId = post.audience.removePrefix("circle:")
        if (circleId == post.audience) return
        val epoch = post.encryptedBody?.let {
            runCatching { JSONObject(it).getLong("epoch") }.getOrNull()
        } ?: return
        val key = store.foldKey(circleId, epoch) ?: return
        viewModelScope.launch {
            runCatching {
                val encrypted = api?.mediaFetch(blobId) ?: throw HiveException("media unavailable")
                val bytes = WireCrypto.decryptFoldContent(key, circleId, String(encrypted))
                if (bytes.size > MESSAGE_MEDIA_MAX_BYTES) throw HiveException("Fold media is too large")
                val extension = when {
                    mime.startsWith("video/") -> "mp4"
                    mime.contains("png") -> "png"
                    else -> "jpg"
                }
                File(postMediaDirectory, "$blobId.$extension").also { file ->
                    FileOutputStream(file).use { it.write(bytes) }
                }
            }.onSuccess { file -> foldMediaFiles = foldMediaFiles + (blobId to file) }
                .onFailure { errorDialog = it.message ?: "Fold media could not be decrypted" }
        }
    }

    fun report(p: Post, reason: String) = run("report failed") {
        api?.report(
            "post",
            p.postId,
            reason,
            reporterCopy = p.body.takeIf { p.audience.startsWith("circle:") },
        )
        status = "Reported — thank you"
    }

    fun reportComment(comment: Comment, reason: String) = run("report failed") {
        val folded = foldPosts.any { post ->
            post.postId == openPostId && post.audience.startsWith("circle:")
        }
        api?.report("comment", comment.commentId, reason, comment.body.takeIf { folded })
        status = "Reported — thank you"
    }

    // --------------------------------------------------------- post detail

    /** Open one post as an overlay (alerts tap-through, permalinks). */
    fun openPostDetail(postId: String, withComments: Boolean = false) = run("post failed") {
        val c = api ?: return@run
        detailPost = parsePost(c.postGet(postId).getJSONObject("post"))
        if (withComments) loadComments(postId)
    }

    fun closePostDetail() {
        detailPost = null
    }

    fun openFoldActivity(kind: String, subjectId: String, foldId: String = "") {
        foldsOpen = true
        run("Fold activity unavailable", loud = true) {
            refreshFolds()
            requestedFoldId = foldId.takeIf { it.isNotEmpty() } ?: when (kind) {
                "fold_post" -> api?.postGet(subjectId)
                    ?.optJSONObject("post")
                    ?.optString("audience")
                    ?.removePrefix("circle:")
                    ?.takeIf { it.isNotEmpty() }
                "fold_joined" -> subjectId.takeIf { it.isNotEmpty() }
                else -> null
            }
        }
    }

    fun consumeRequestedFold() {
        requestedFoldId = null
    }

    // --------------------------------------------------------------- folds

    private suspend fun wrapFoldKeyFor(
        account: String,
        circleId: String,
        epoch: Long,
        key: ByteArray,
    ): JSONObject {
        val c = api ?: throw HiveException("HIVE is unavailable")
        val directory = c.foldKeyDirectory(account)
        val accountId = directory.getString("account_id")
        val identityPub = directory.getString("identity_pub")
        val devices = directory.getJSONArray("devices")
        if (devices.length() == 0) throw HiveException("@$account has no encrypted device key")
        return JSONObject().apply {
            for (index in 0 until devices.length()) {
                val device = devices.getJSONObject(index)
                if (!WireCrypto.validateDevice(accountId, identityPub, device)) {
                    throw HiveException("@$account has an invalid device credential")
                }
                put(
                    device.getString("device_id"),
                    WireCrypto.wrapFoldKey(
                        key,
                        circleId,
                        epoch,
                        unb64(device.getString("wire_pub")),
                    ),
                )
            }
        }
    }

    private fun unwrapFoldKey(circleId: String, epoch: Long, wrapped: JSONObject?): ByteArray? {
        store.foldKey(circleId, epoch)?.let { return it }
        val e = enrollment ?: return null
        val deviceId = deviceIdFor(e.device.publicBytes)
        val envelope = wrapped?.optString(deviceId)?.takeIf { it.isNotEmpty() } ?: return null
        return runCatching {
            WireCrypto.unwrapFoldKey(store.wireKey(), circleId, epoch, envelope).also {
                store.saveFoldKey(circleId, epoch, it)
            }
        }.getOrNull()
    }

    private fun parseFoldMembers(array: JSONArray?): List<Author> =
        (0 until (array?.length() ?: 0)).mapNotNull { index ->
            array!!.optJSONObject(index)?.optJSONObject("account")?.let(Author::of)
        }

    private suspend fun bootstrapOwnedFold(fold: Fold): Fold {
        if (fold.keyEpoch <= 0) return fold
        if (store.foldKey(fold.circleId, fold.keyEpoch) != null) return fold
        unwrapFoldKey(fold.circleId, fold.keyEpoch, fold.wrappedKey)?.let { return fold }
        return try {
            val c = api ?: return fold
            val nextEpoch = fold.keyEpoch + 1
            val key = WireCrypto.generateFoldKey()
            val accounts = (fold.members.map { it.accountId } + accountId).distinct()
            val wrapped = JSONObject()
            accounts.forEach { id ->
                wrapped.put(id, wrapFoldKeyFor(id, fold.circleId, nextEpoch, key))
            }
            val appliedEpoch = c.circleSetKeys(fold.circleId, fold.keyEpoch, wrapped)
            store.saveFoldKey(fold.circleId, appliedEpoch, key)
            fold.copy(
                keyEpoch = appliedEpoch,
                wrappedKey = wrapped.optJSONObject(accountId),
                wrappedKeys = wrapped,
            )
        } catch (error: Exception) {
            status = "${fold.name}: ${error.message ?: "secure member keys are not ready"}"
            fold
        }
    }

    private suspend fun syncOwnedFoldDevices(fold: Fold): Fold {
        if (fold.keyEpoch <= 0 || fold.wrappedKeys == null) return fold
        val key = store.foldKey(fold.circleId, fold.keyEpoch) ?: return fold
        var rotationNeeded = false
        for (member in fold.members) {
            val directory = api?.foldKeyDirectory(member.accountId) ?: continue
            val devices = directory.optJSONArray("devices")
            if ((devices?.length() ?: 0) == 0) {
                status = "${fold.name}: @${member.handle} must open Connect before encrypted activity can start"
                return fold
            }
            val wrapped = fold.wrappedKeys.optJSONObject(member.accountId)
            val expected = (0 until (devices?.length() ?: 0)).map {
                devices!!.getJSONObject(it).getString("device_id")
            }.toSet()
            val present = wrapped?.keys()?.asSequence()?.toSet().orEmpty()
            if (!present.containsAll(expected)) {
                rotationNeeded = true
                break
            }
        }
        if (!rotationNeeded) return fold
        val nextEpoch = fold.keyEpoch + 1
        val nextKey = WireCrypto.generateFoldKey()
        val wrapped = JSONObject()
        fold.members.forEach { member ->
            wrapped.put(
                member.accountId,
                wrapFoldKeyFor(member.accountId, fold.circleId, nextEpoch, nextKey),
            )
        }
        val appliedEpoch = api?.circleSetKeys(fold.circleId, fold.keyEpoch, wrapped) ?: return fold
        store.saveFoldKey(fold.circleId, appliedEpoch, nextKey)
        return fold.copy(
            keyEpoch = appliedEpoch,
            wrappedKey = wrapped.optJSONObject(accountId),
            wrappedKeys = wrapped,
        )
    }

    fun refreshFolds() = run("folds failed") {
        val c = api ?: return@run
        val v = c.circles()
        encryptedFoldsReady = v.has("invited")
        val own = v.optJSONArray("own")
        val member = v.optJSONArray("member")
        val list = mutableListOf<Fold>()
        for (i in 0 until (own?.length() ?: 0)) {
            val o = own!!.getJSONObject(i)
            val parsedMembers = if (o.has("members")) parseFoldMembers(o.optJSONArray("members")) else {
                val known = (followers + following).associateBy { it.accountId }
                o.optJSONObject("wrapped_keys")?.keys()?.asSequence()
                    ?.mapNotNull(known::get)?.toList().orEmpty()
            }
            val ownerCard = myProfile?.let {
                Author(it.accountId, it.handle, it.displayName, it.avatarBlob)
            }
            val activeMembers = if (ownerCard != null && parsedMembers.none { it.accountId == accountId }) {
                listOf(ownerCard) + parsedMembers
            } else parsedMembers
            val fold = Fold(
                circleId = o.getString("circle_id"),
                name = o.optString("name"),
                owned = true,
                members = activeMembers,
                pending = (0 until (o.optJSONArray("pending")?.length() ?: 0)).mapNotNull { index ->
                    o.optJSONArray("pending")?.optJSONObject(index)?.optJSONObject("account")
                        ?.let(Author::of)
                },
                keyEpoch = if (o.has("key_epoch")) o.optLong("key_epoch", 1) else 0,
                wrappedKey = o.optJSONObject("wrapped_key"),
                wrappedKeys = o.optJSONObject("wrapped_keys"),
            )
            list += syncOwnedFoldDevices(bootstrapOwnedFold(fold))
        }
        for (i in 0 until (member?.length() ?: 0)) {
            val m = member!!.getJSONObject(i)
            list += Fold(
                circleId = m.getString("circle_id"),
                name = m.optString("name"),
                owned = false,
                members = parseFoldMembers(m.optJSONArray("members")),
                ownerLabel = m.optJSONObject("owner")?.let { Author.of(it).label } ?: "",
                keyEpoch = if (m.has("key_epoch")) m.optLong("key_epoch", 1) else 0,
                wrappedKey = m.optJSONObject("wrapped_key"),
                joinedAt = m.optLong("joined_at").takeIf { it > 0 },
            )
            unwrapFoldKey(list.last().circleId, list.last().keyEpoch, list.last().wrappedKey)
        }
        val invited = v.optJSONArray("invited")
        for (i in 0 until (invited?.length() ?: 0)) {
            val value = invited!!.getJSONObject(i)
            list += Fold(
                circleId = value.getString("circle_id"),
                name = value.optString("name"),
                owned = false,
                ownerLabel = value.optJSONObject("owner")?.let { Author.of(it).label } ?: "",
                invited = true,
            )
        }
        folds = list
    }

    fun createFold(name: String) {
        if (name.isBlank()) return
        run("fold create failed", loud = true) {
            val c = api ?: return@run
            val circleId = c.circleCreate(name.trim(), JSONObject())
            if (!encryptedFoldsReady) {
                refreshFolds()
                status = "Fold created; encrypted activity requires the pending HIVE update"
                return@run
            }
            val key = WireCrypto.generateFoldKey()
            val nextEpoch = 2L
            val wrapped = JSONObject().put(
                accountId,
                wrapFoldKeyFor(accountId, circleId, nextEpoch, key),
            )
            val epoch = c.circleSetKeys(circleId, 1, wrapped)
            store.saveFoldKey(circleId, epoch, key)
            refreshFolds()
            status = "Fold created"
        }
    }

    fun foldInvite(fold: Fold, member: Author) = run("Fold invitation failed", loud = true) {
        val c = api ?: return@run
        val key = store.foldKey(fold.circleId, fold.keyEpoch)
            ?: throw HiveException("Fold key is unavailable on this device")
        c.foldInvite(
            fold.circleId,
            member.accountId,
            wrapFoldKeyFor(member.accountId, fold.circleId, fold.keyEpoch, key),
        )
        refreshFolds()
    }

    fun foldRemoveMember(fold: Fold, member: Author) = run("fold update failed") {
        val c = api ?: return@run
        c.foldRemove(fold.circleId, member.accountId)
        val remaining = fold.members.filter { it.accountId != member.accountId }
        val nextEpoch = fold.keyEpoch + 1
        val key = WireCrypto.generateFoldKey()
        val wrapped = JSONObject()
        (remaining.map { it.accountId } + accountId).distinct().forEach { id ->
            wrapped.put(id, wrapFoldKeyFor(id, fold.circleId, nextEpoch, key))
        }
        val epoch = c.circleSetKeys(fold.circleId, fold.keyEpoch, wrapped)
        store.saveFoldKey(fold.circleId, epoch, key)
        refreshFolds()
    }

    fun foldAccept(fold: Fold) = run("Fold acceptance failed", loud = true) {
        api?.foldAccept(fold.circleId)
        refreshFolds()
    }

    fun foldDecline(fold: Fold) = run("Fold decline failed") {
        api?.foldDecline(fold.circleId)
        refreshFolds()
    }

    fun foldLeave(fold: Fold) = run("Fold leave failed", loud = true) {
        api?.foldLeave(fold.circleId)
        store.removeFoldKeys(fold.circleId)
        refreshFolds()
    }

    fun deleteFold(fold: Fold) = run("fold delete failed", loud = true) {
        api?.circleDelete(fold.circleId)
        refreshFolds()
        refreshFeed()
        refreshMyPosts()
    }

    /** Fold name for a post audience tag ("circle:<id>" → name). */
    fun foldName(audience: String): String? {
        val id = audience.removePrefix("circle:")
        return if (id == audience) null else folds.find { it.circleId == id }?.name
    }

    // ------------------------------------------------------------- invites

    fun refreshInvites() = run("invites failed") {
        val c = api ?: return@run
        val arr = c.communityInviteList().optJSONArray("invites")
        invites = (0 until (arr?.length() ?: 0)).map { i ->
            val o = arr!!.getJSONObject(i)
            InviteRow(
                code = o.getString("code"),
                link = o.optString("link", ""),
                created = o.optLong("created"),
                usedHandle = o.strOrNull("used_handle"),
                usedAt = if (o.isNull("used_at")) null else o.optLong("used_at"),
            )
        }
    }

    fun mintInvite() = run("invite failed", loud = true) {
        val codes = api?.communityInviteCreate(1) ?: return@run
        status = if (codes.isEmpty()) "" else "Invite created"
        refreshInvites()
    }

    fun revokeInvite(code: String) = run("invite revoke failed") {
        api?.communityInviteRevoke(code)
        refreshInvites()
    }

    // ------------------------------------------------------------ support

    fun refreshSupportThreads() = run("support failed") {
        val value = api?.supportThreads() ?: return@run
        val array = value.optJSONArray("threads")
        supportThreads = (0 until (array?.length() ?: 0)).map { index ->
            val thread = array!!.getJSONObject(index)
            val messages = thread.optJSONArray("messages")
            SupportThread(
                id = thread.getString("id"),
                category = thread.optString("category", "other"),
                subject = thread.optString("subject"),
                status = thread.optString("status", "open"),
                updated = thread.optLong("updated"),
                messages = (0 until (messages?.length() ?: 0)).map { messageIndex ->
                    val message = messages!!.getJSONObject(messageIndex)
                    SupportMessage(
                        id = message.getString("id"),
                        senderRole = message.optString("sender_role"),
                        body = message.optString("body"),
                        created = message.optLong("created"),
                    )
                },
            )
        }
    }

    fun openSupportThread(category: String, subject: String, body: String, onDone: () -> Unit) {
        if (subject.isBlank() || body.isBlank()) return
        run("support request failed", loud = true) {
            api?.supportOpen(category, subject.trim(), body.trim())
            refreshSupportThreads()
            onDone()
        }
    }

    fun sendSupportReply(threadId: String, body: String, onDone: () -> Unit) {
        if (body.isBlank()) return
        run("support reply failed", loud = true) {
            api?.supportSend(threadId, body.trim())
            refreshSupportThreads()
            onDone()
        }
    }

    // ------------------------------------------------------------ comments

    fun loadComments(postId: String) = run("comments failed") {
        val c = api ?: return@run
        val v = c.comments(postId)
        val arr = v.optJSONArray("comments")
        val host = (posts + foldPosts + myPosts + viewedPosts).firstOrNull { it.postId == postId }
        openComments = if (arr == null) listOf() else (0 until arr.length()).map { i ->
            val x = arr.getJSONObject(i)
            val body = if (host?.audience?.startsWith("circle:") == true) {
                decryptFoldPayload(host.audience, x.optString("body"))?.optString("body")
                    ?: "Encrypted Fold comment unavailable on this device"
            } else x.optString("body")
            Comment(
                x.getString("comment_id"),
                Author.of(x.getJSONObject("author")),
                body,
                x.optLong("created"),
                x.strOrNull("parent_id"),
                x.optLong("reactions"),
                x.strOrNull("my_reaction"),
                if (x.isNull("edited")) null else x.optLong("edited"),
            )
        }
        openPostId = postId
    }

    fun closeComments() {
        openPostId = null
        openComments = listOf()
    }

    fun addComment(
        postId: String,
        body: String,
        parentId: String? = null,
        onDone: () -> Unit = {},
    ) {
        if (body.isBlank()) return
        run("comment failed", loud = true) {
            val host = (posts + foldPosts + myPosts + viewedPosts).firstOrNull { it.postId == postId }
            if (host?.audience?.startsWith("circle:") == true) {
                val circleId = host.audience.removePrefix("circle:")
                val fold = folds.firstOrNull { it.circleId == circleId }
                    ?: throw HiveException("Fold membership is unavailable")
                val key = store.foldKey(circleId, fold.keyEpoch)
                    ?: throw HiveException("Fold key is unavailable on this device")
                val plaintext = JSONObject().put("body", body.trim()).toString().toByteArray()
                val envelope = WireCrypto.encryptFoldContent(
                    key,
                    circleId,
                    fold.keyEpoch,
                    plaintext,
                )
                api?.commentCreate(postId, envelope, parentId, fold.keyEpoch)
            } else {
                api?.commentCreate(postId, body.trim(), parentId)
            }
            loadComments(postId)
            if (host?.audience?.startsWith("circle:") == true) {
                folds.firstOrNull { "circle:${it.circleId}" == host.audience }?.let { refreshFoldFeed(it) }
            } else refreshFeed()
            onDone()
        }
    }

    /** Toggle ❤️ on a comment; optimistic like posts. */
    fun toggleCommentLike(c: Comment) {
        val clearing = c.myReaction != null
        val updated = c.copy(
            myReaction = if (clearing) null else "❤️",
            reactions = if (clearing) (c.reactions - 1).coerceAtLeast(0) else c.reactions + 1,
        )
        openComments = openComments.map { if (it.commentId == c.commentId) updated else it }
        run("reaction failed") {
            if (clearing) api?.commentUnreact(c.commentId) else api?.commentReact(c.commentId)
        }
    }

    // ------------------------------------------------------------ settings

    fun openSettings() {
        settingsOpen = true
        recentlyAccepted = listOf()
        run("settings failed") {
            val v = api?.settingsGet() ?: return@run
            setDiscoverable = v.optBoolean("discoverable", true)
            setAutoAccept = v.optBoolean("auto_accept", false)
            setCommentsFrom = v.optString("comments_from", "viewers")
            refreshGraph()
        }
    }

    fun saveSettings(discoverable: Boolean, autoAccept: Boolean, commentsFrom: String) {
        setDiscoverable = discoverable
        setAutoAccept = autoAccept
        setCommentsFrom = commentsFrom
        run("settings failed") {
            api?.settingsSet(discoverable, autoAccept, commentsFrom)
            status = "Settings saved"
        }
    }

    fun changeHandle(newHandle: String) {
        val h = newHandle.trim().removePrefix("@")
        if (h.isEmpty() || h == handle) return
        run("handle change failed", loud = true) {
            api?.handleSet(h)
            handle = h
            store.updateHandle(h)
            refreshMyProfile()
            status = "You are now @$h"
        }
    }

    // --------------------------------------------------------------- graph

    private suspend fun refreshGraph() {
        val c = api ?: return
        val v = c.follows()
        fun list(key: String): List<Author> {
            val arr = v.optJSONArray(key) ?: return listOf()
            return (0 until arr.length()).map { Author.of(arr.getJSONObject(it)) }
        }
        following = list("following")
        followers = list("followers")
        pendingIn = list("pending_in")
        pendingOut = list("pending_out")
    }

    fun requestFollow(target: String) {
        if (target.isBlank()) return
        run("request failed") {
            api?.followRequest(target.trim())
            status = "Request sent"
            refreshGraph()
            reloadOpenViews(target)
        }
    }

    fun accept(a: Author) = run("accept failed") {
        api?.followAccept(a.accountId)
        recentlyAccepted = recentlyAccepted.filter { it.accountId != a.accountId } + a
        refreshGraph()
        refreshAlerts()
    }

    fun decline(a: Author) = run("decline failed") {
        api?.followDecline(a.accountId)
        refreshGraph()
        refreshAlerts()
    }

    fun unfollow(a: Author) = run("unfollow failed") {
        api?.unfollow(a.accountId)
        refreshGraph()
        refreshFeed()
        reloadOpenViews(a.accountId)
    }

    fun block(a: Author) = run("block failed") {
        api?.block(a.accountId)
        refreshGraph()
        refreshFeed()
        refreshAlerts()
        if (viewedProfile?.accountId == a.accountId) closeProfile()
    }

    /** After a graph change, keep any open profile/search views in sync. */
    private suspend fun reloadOpenViews(target: String) {
        viewedProfile?.let {
            if (it.accountId == target || it.handle == target) loadProfileInto(target)
        }
        if (searchResults.isNotEmpty()) refreshSearchStates()
    }

    // -------------------------------------------------------------- alerts

    private suspend fun refreshAlerts(notifyNew: Boolean = false) {
        val c = api ?: return
        val v = c.notifications(null, 50)
        val arr = v.optJSONArray("notifications")
        alerts = if (arr == null) listOf() else (0 until arr.length()).map { i ->
            val x = arr.getJSONObject(i)
            Alert(
                x.getString("id"),
                x.optString("kind"),
                Author.of(x.getJSONObject("from")),
                x.optString("subject_id"),
                x.optLong("created"),
                x.optBoolean("seen"),
                title = x.optString("title"),
                body = x.optString("body"),
                announceKind = x.optString("announce_kind"),
                foldId = x.optString("fold_id"),
            )
        }
        unseen = v.optLong("unseen")
        // Freshness (the consume-once dedupe set) is only spent on paths that
        // actually POST a notification. A plain refresh (startup, pull) must
        // never consume an alert's freshness — that silently suppressed the
        // background notification for any alert that arrived while the app
        // happened to be open. Seen alerts never notify; opening the Alerts
        // tab absorbs them (markAlertsSeen).
        if (notifyNew) {
            val unseenAlerts = alerts.filter { !it.seen }
            val fresh = store.newNotificationIds("alert", unseenAlerts.map { it.id })
            val failed = unseenAlerts.filter { it.id in fresh }
                .filterNot { ConnectNotifications.postAlert(getApplication(), it) }
            store.retryNotificationIds("alert", failed.map { it.id })
        }
    }

    /** Opening the Alerts tab clears the badge. */
    fun markAlertsSeen() {
        // Absorb every visible alert into the notified set FIRST: the user is
        // looking at them, so no path (foreground or background worker) should
        // ever post a system notification for them again.
        store.newNotificationIds("alert", alerts.map { it.id })
        if (unseen == 0L) return
        run("alerts failed") {
            api?.notificationsSeen()
            unseen = 0
            alerts = alerts.map { it.copy(seen = true) }
        }
    }

    fun deleteAlert(id: String) {
        run("alert delete failed") {
            api?.notificationDelete(id)
            val removed = alerts.firstOrNull { it.id == id }
            alerts = alerts.filter { it.id != id }
            if (removed?.seen == false && unseen > 0) unseen -= 1
        }
    }

    // --------------------------------------------------------------- legal

    /** Refresh the ToS/PP acceptance state; opens the blocking review gate
     *  when the server has newer document versions than this account
     *  accepted. Failures fail open (never lock the user out on a network
     *  blip) — the server-side acceptance record is the legal source of
     *  truth, not this gate. */
    private suspend fun refreshLegalStatus(c: HiveClient) {
        val v = runCatching { c.legalStatus() }.getOrNull() ?: return
        val legal = v.optJSONObject("legal") ?: return
        val arr = legal.optJSONArray("docs")
        legalDocs = if (arr == null) listOf() else (0 until arr.length()).map { i ->
            val x = arr.getJSONObject(i)
            LegalDoc(x.optString("doc"), x.optString("version"), x.optBoolean("accepted"))
        }
        legalGateNeeded = legal.optBoolean("needs_acceptance")
    }

    /** Explicit acceptance from the review gate; records server-side. */
    fun acceptLegalDocuments() {
        val c = api ?: return
        viewModelScope.launch {
            legalAccepting = true
            try {
                val v = c.legalAccept()
                val legal = v.optJSONObject("legal")
                legalGateNeeded = legal?.optBoolean("needs_acceptance") ?: false
                if (!legalGateNeeded) status = "Updated terms accepted"
            } catch (e: Exception) {
                status = e.message ?: "could not record acceptance"
            } finally {
                legalAccepting = false
            }
        }
    }

    /** Fetch a served legal document ("terms" | "privacy") as plain text.
     *  Works before enrollment too (unauthenticated endpoint) — pass the
     *  server from the sign-up form when not signed in. */
    suspend fun fetchLegalDocument(doc: String, serverOverride: String? = null): String {
        api?.let { return it.legalDocument(doc) }
        val srv = serverOverride?.trim()?.removeSuffix("/")
        if (srv.isNullOrEmpty()) throw HiveException("no server")
        return HiveClient(srv, acceptSelfSigned = BuildConfig.DEBUG).legalDocument(doc)
    }

    // ------------------------------------------------------------ discover

    fun search(q: String) {
        if (q.isBlank()) {
            searchResults = listOf()
            return
        }
        viewModelScope.launch {
            searching = true
            try {
                val c = api ?: return@launch
                val v = c.search(q.trim())
                val arr = v.optJSONArray("results")
                searchResults = if (arr == null) listOf() else (0 until arr.length()).map { i ->
                    val x = arr.getJSONObject(i)
                    SearchHit(Author.of(x), x.strOrNull("follow_state"))
                }
            } catch (e: Exception) {
                status = e.message ?: "search failed"
            } finally {
                searching = false
            }
        }
    }

    private suspend fun refreshSearchStates() {
        val c = api ?: return
        searchResults = searchResults.map { hit ->
            val p = runCatching { c.profileGet(hit.author.accountId) }.getOrNull()
            hit.copy(followState = p?.strOrNull("follow_state"))
        }
    }

    // ------------------------------------------------------------ profiles

    private fun parseProfile(v: JSONObject) = Profile(
        accountId = v.optString("account_id"),
        handle = v.optString("handle"),
        displayName = v.optString("display_name"),
        bio = v.optString("bio"),
        avatarBlob = v.strOrNull("avatar_blob"),
        founder = v.optBoolean("founder"),
        communityRole = v.optString("community_role", "member"),
        membershipTier = v.optString("membership_tier", "beta"),
        followState = v.strOrNull("follow_state"),
        followers = v.optLong("followers"),
        following = v.optLong("following"),
        postCount = v.optLong("posts"),
    )

    private suspend fun refreshMyProfile() {
        val c = api ?: return
        myProfile = runCatching { parseProfile(c.profileGet(accountId)) }.getOrNull()
        refreshMyPosts()
    }

    private suspend fun refreshMyPosts() {
        val c = api ?: return
        if (accountId.isEmpty()) return
        myPosts = profilePostOrder(parsePosts(c.authorPosts(accountId, null, 30)))
    }

    private suspend fun loadProfileInto(target: String) {
        val c = api ?: return
        viewedProfile = parseProfile(c.profileGet(target))
        reloadViewedPosts()
    }

    private suspend fun reloadViewedPosts() {
        val c = api ?: return
        val p = viewedProfile ?: return
        viewedPosts = profilePostOrder(parsePosts(c.authorPosts(p.accountId, null, 30)))
    }

    private fun profilePostOrder(value: List<Post>): List<Post> = value.sortedWith(
        compareByDescending<Post> { it.pinned }.thenByDescending { it.created },
    )

    /** Open someone's profile as an overlay (from search, feed, alerts). */
    fun openProfile(target: String) = run("profile failed") {
        if (target == accountId) return@run // own profile lives in its tab
        loadProfileInto(target)
    }

    fun closeProfile() {
        viewedProfile = null
        viewedPosts = listOf()
    }

    fun saveProfile(displayName: String, bio: String, avatarUri: Uri?) {
        run("profile save failed") {
            val c = api ?: return@run
            val avatarBlob = when {
                avatarUri != null -> {
                    status = "Uploading avatar…"
                    c.blobUpload(processImage(avatarUri), purpose = "connect_media")
                }
                else -> myProfile?.avatarBlob
            }
            status = ""
            c.profileSet(displayName.trim(), bio.trim(), avatarBlob)
            refreshMyProfile()
            refreshFeed()
        }
    }

    override fun onCleared() {
        CallSession.setVisible(false)
        messageMediaDirectory.deleteRecursively()
        postMediaDirectory.deleteRecursively()
        File(getApplication<Application>().cacheDir, "wire-recordings").deleteRecursively()
        super.onCleared()
    }

}

private const val MESSAGE_MEDIA_MAX_BYTES = 25 * 1_024 * 1_024
