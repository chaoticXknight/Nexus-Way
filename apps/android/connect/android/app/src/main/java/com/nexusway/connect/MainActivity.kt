// Owns the Android activity, secure window, notification destinations, theme,
// and top-level Compose navigation. It does not own network or durable state;
// ConnectViewModel and Store provide those boundaries.

package com.nexusway.connect

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Outline
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.LayoutDirection
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.viewmodel.compose.viewModel
import coil.ImageLoader
import coil.compose.LocalImageLoader
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.text.TextStyle
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.sin

private const val DefaultHiveServer = "https://dev.nexus-way.net"

private fun fmtBytes(bytes: Long): String {
    val mb = bytes / (1024.0 * 1024.0)
    return if (mb >= 10) "${mb.toInt()} MB" else String.format(java.util.Locale.US, "%.1f MB", mb)
}

private fun fmtSpeed(bytesPerSecond: Long): String = if (bytesPerSecond <= 0L) {
    "-- MB/s"
} else {
    "${fmtBytes(bytesPerSecond)}/s"
}

private fun fmtEta(seconds: Long?): String = when {
    seconds == null -> "ETA --"
    seconds < 60 -> "ETA ${seconds}s"
    else -> "ETA ${seconds / 60}m ${seconds % 60}s"
}

data class ConnectPalette(
    val bg: Color,
    val panel: Color,
    val panelHi: Color,
    val panelHover: Color,
    val border: Color,
    val borderSoft: Color,
    val accent: Color,
    val accent2: Color,
    val honey: Color,
    val accentSoft: Color,
    val textMain: Color,
    val textDim: Color,
    val textMuted: Color,
    val danger: Color,
)

private val DarkPalette = ConnectPalette(
    bg = Color(0xFF0A0C11),
    panel = Color(0xFF11141B),
    panelHi = Color(0xFF161A23),
    panelHover = Color(0xFF1D222D),
    border = Color(0xFF262C3A),
    borderSoft = Color(0xFF1C2230),
    accent = Color(0xFF6D5EFC),
    accent2 = Color(0xFF18C8F5),
    honey = Color(0xFFF2B84B),
    accentSoft = Color(0x266D5EFC),
    textMain = Color(0xFFF4F6FB),
    textDim = Color(0xFF9AA4B6),
    textMuted = Color(0xFF5E6878),
    danger = Color(0xFFFF5D6A),
)

private val LightPalette = ConnectPalette(
    bg = Color(0xFFFFFCF4),
    panel = Color(0xF7FFFFFF),
    panelHi = Color(0xFFFFF7E0),
    panelHover = Color(0xFFFFF1C7),
    border = Color(0xFFD7B85E),
    borderSoft = Color(0xFFEBDDAF),
    accent = Color(0xFFB8860B),
    accent2 = Color(0xFFD6A21E),
    honey = Color(0xFFF2B84B),
    accentSoft = Color(0x33D6A21E),
    textMain = Color(0xFF19140A),
    textDim = Color(0xFF6E5A22),
    textMuted = Color(0xFF9A8340),
    danger = Color(0xFFB42318),
)

val LocalConnectPalette = staticCompositionLocalOf { DarkPalette }
val Bg @Composable get() = LocalConnectPalette.current.bg
val Panel @Composable get() = LocalConnectPalette.current.panel
val PanelHi @Composable get() = LocalConnectPalette.current.panelHi
val PanelHover @Composable get() = LocalConnectPalette.current.panelHover
val Border @Composable get() = LocalConnectPalette.current.border
val BorderSoft @Composable get() = LocalConnectPalette.current.borderSoft
val Accent @Composable get() = LocalConnectPalette.current.accent
val Accent2 @Composable get() = LocalConnectPalette.current.accent2
val Honey @Composable get() = LocalConnectPalette.current.honey
val AccentSoft @Composable get() = LocalConnectPalette.current.accentSoft
val TextMain @Composable get() = LocalConnectPalette.current.textMain
val TextDim @Composable get() = LocalConnectPalette.current.textDim
val TextMuted @Composable get() = LocalConnectPalette.current.textMuted
val Danger @Composable get() = LocalConnectPalette.current.danger

/** The suite's brand gradient (135° purple → cyan). */
val BrandBrush @Composable get() = Brush.linearGradient(listOf(Accent, Accent2))
val BrandTextStyle @Composable get() = TextStyle(brush = BrandBrush)

private val DarkColors = darkColorScheme(
    primary = DarkPalette.accent,
    onPrimary = DarkPalette.textMain,
    background = DarkPalette.bg,
    onBackground = DarkPalette.textMain,
    surface = DarkPalette.panel,
    onSurface = DarkPalette.textMain,
    surfaceVariant = DarkPalette.panelHi,
    onSurfaceVariant = DarkPalette.textDim,
    surfaceContainer = DarkPalette.panel,
    surfaceContainerHigh = DarkPalette.panelHi,
    surfaceContainerHighest = DarkPalette.panelHi,
    secondaryContainer = Color(0xFF232A38), // Theme.bg-active
    onSecondaryContainer = DarkPalette.textMain,
    outline = DarkPalette.border,
    outlineVariant = DarkPalette.borderSoft,
    error = DarkPalette.danger,
)

private val LightColors = lightColorScheme(
    primary = LightPalette.accent,
    onPrimary = Color.White,
    background = LightPalette.bg,
    onBackground = LightPalette.textMain,
    surface = LightPalette.panel,
    onSurface = LightPalette.textMain,
    surfaceVariant = LightPalette.panelHi,
    onSurfaceVariant = LightPalette.textDim,
    surfaceContainer = LightPalette.panel,
    surfaceContainerHigh = LightPalette.panelHi,
    surfaceContainerHighest = LightPalette.panelHi,
    secondaryContainer = LightPalette.accentSoft,
    onSecondaryContainer = LightPalette.textMain,
    outline = LightPalette.border,
    outlineVariant = LightPalette.borderSoft,
    error = LightPalette.danger,
)

class MainActivity : ComponentActivity() {
    private var notificationDestination by mutableStateOf<String?>(null)
    private var notificationCallId by mutableStateOf<String?>(null)
    private var notificationAlertKind by mutableStateOf<String?>(null)
    private var notificationAlertSubject by mutableStateOf<String?>(null)
    private var notificationFoldId by mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        restoreFallbackCall(intent)
        ConnectNotifications.clearDelivered(this)
        notificationDestination = intent?.action?.takeIf {
            it == OPEN_ALERTS_ACTION || it == OPEN_MESSAGES_ACTION ||
                it == OPEN_CALL_ACTION || it == OPEN_FOLD_ACTION
        }
        notificationCallId = intent?.getStringExtra(CALL_ID_EXTRA)
        notificationAlertKind = intent?.getStringExtra(ALERT_KIND_EXTRA)
        notificationAlertSubject = intent?.getStringExtra(ALERT_SUBJECT_EXTRA)
        notificationFoldId = intent?.getStringExtra(FOLD_ID_EXTRA)
        window.setFlags(
            WindowManager.LayoutParams.FLAG_SECURE,
            WindowManager.LayoutParams.FLAG_SECURE,
        )
        setContent {
            val vm: ConnectViewModel = viewModel()
            val palette = if (vm.themeMode == "light") LightPalette else DarkPalette
            CompositionLocalProvider(LocalConnectPalette provides palette) {
            MaterialTheme(colorScheme = if (vm.themeMode == "light") LightColors else DarkColors) {
                Surface(Modifier.fillMaxSize(), color = Bg) {
                    Box(
                        Modifier
                            .fillMaxSize()
                            .windowInsetsPadding(WindowInsets.safeDrawing),
                    ) {
                        HiveHoneycombBackground()
                        Root(
                            vm,
                            notificationDestination,
                            notificationCallId,
                            notificationAlertKind,
                            notificationAlertSubject,
                            notificationFoldId,
                        ) {
                            notificationDestination = null
                            notificationCallId = null
                            notificationAlertKind = null
                            notificationAlertSubject = null
                            notificationFoldId = null
                        }
                    }
                }
            }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        restoreFallbackCall(intent)
        ConnectNotifications.clearDelivered(this)
        setIntent(intent)
        notificationDestination = intent.action?.takeIf {
            it == OPEN_ALERTS_ACTION || it == OPEN_MESSAGES_ACTION ||
                it == OPEN_CALL_ACTION || it == OPEN_FOLD_ACTION
        }
        notificationCallId = intent.getStringExtra(CALL_ID_EXTRA)
        notificationAlertKind = intent.getStringExtra(ALERT_KIND_EXTRA)
        notificationAlertSubject = intent.getStringExtra(ALERT_SUBJECT_EXTRA)
        notificationFoldId = intent.getStringExtra(FOLD_ID_EXTRA)
    }

    override fun onResume() {
        super.onResume()
        NexusNotifier.ensureRunning(this)
        ConnectNotifications.clearDelivered(this)
    }

    private fun restoreFallbackCall(intent: Intent?) {
        if (intent?.action != OPEN_CALL_ACTION) return
        if (intent.getLongExtra("call_expires_at", 0L) <= System.currentTimeMillis() / 1_000) return
        val frame = intent.getStringExtra("call_frame")
            ?.let { runCatching { org.json.JSONObject(it) }.getOrNull() } ?: return
        CallSession.receive(frame)
    }
}

object HiveCutShape : Shape {
    override fun createOutline(size: Size, layoutDirection: LayoutDirection, density: Density) =
        Outline.Generic(path(size))

    fun path(size: Size): Path = Path().apply {
        val cut = 18f
        moveTo(cut, 0f)
        lineTo(size.width, 0f)
        lineTo(size.width, size.height - cut)
        lineTo(size.width - cut, size.height)
        lineTo(0f, size.height)
        lineTo(0f, cut)
        close()
    }
}

@Composable
fun Modifier.hivePanelDepth(active: Boolean = false): Modifier {
    val accent2 = Accent2
    return this.drawBehind {
    val shadow = HiveCutShape.path(size).apply { translate(Offset(0f, if (active) 10f else 7f)) }
    drawPath(shadow, color = Color.Black.copy(alpha = if (active) 0.32f else 0.22f))
    drawPath(HiveCutShape.path(size), color = accent2.copy(alpha = 0.24f), style = Stroke(1.4f))
    drawRect(
        Brush.verticalGradient(listOf(Color.White.copy(alpha = 0.10f), Color.Transparent)),
        size = Size(size.width, size.height * 0.45f),
    )
}
}

@Composable
fun HiveHoneycombBackground() {
    val palette = LocalConnectPalette.current
    val transition = rememberInfiniteTransition(label = "hive-bg")
    val time by transition.animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = androidx.compose.animation.core.infiniteRepeatable(
            androidx.compose.animation.core.tween(9000, easing = androidx.compose.animation.core.LinearEasing),
        ),
        label = "hive-bg-time",
    )
    Canvas(Modifier.fillMaxSize()) {
        drawRect(
            Brush.radialGradient(
                colors = if (palette == LightPalette) {
                    listOf(Color.White, Color(0xFFFFF7E0), palette.bg)
                } else {
                    listOf(Color(0xFF151B30), Color(0xFF0B1020), palette.bg)
                },
                center = Offset(size.width * 0.5f, size.height * 0.32f),
                radius = size.maxDimension,
            ),
        )
        drawHoneycomb(this, 0.20f, 52f, time, palette.accent2, palette.honey)
    }
}

    private fun drawHoneycomb(scope: DrawScope, alpha: Float, cell: Float, time: Float, accent2: Color, honey: Color) = with(scope) {
    val stepX = 1.732f * cell
    val stepY = 1.5f * cell
    val rows = (size.height / stepY).toInt() + 4
    val cols = (size.width / stepX).toInt() + 4
    for (row in -1..rows) {
        for (col in -1..cols) {
            val center = Offset(col * stepX + if (row % 2 == 0) 0f else stepX / 2f, row * stepY)
            val glow = 0.45f + 0.55f * sin((row + col) * 0.75f + time * PI.toFloat() * 2f)
            drawPath(hexPath(center + Offset(0f, cell * 0.10f), cell), color = Color.Black.copy(alpha = alpha * 0.34f), style = Stroke(2.2f))
            drawPath(hexPath(center, cell), color = accent2.copy(alpha = alpha * (0.45f + glow * 0.55f)), style = Stroke(1.1f))
            if ((row + col) % 7 == 0) drawCircle(honey.copy(alpha = alpha * 1.5f), 2.6f, center)
        }
    }
}

private fun hexPath(center: Offset, radius: Float): Path = Path().apply {
    for (index in 0..6) {
        val angle = PI / 6 + index * PI / 3
        val point = Offset(center.x + cos(angle).toFloat() * radius, center.y + sin(angle).toFloat() * radius)
        if (index == 0) moveTo(point.x, point.y) else lineTo(point.x, point.y)
    }
    close()
}

@Suppress("DEPRECATION") // LocalImageLoader provider is the stable Coil 2 API
@Composable
fun Root(
    vm: ConnectViewModel,
    notificationDestination: String?,
    notificationCallId: String?,
    notificationAlertKind: String?,
    notificationAlertSubject: String?,
    notificationFoldId: String?,
    onNotificationDestinationConsumed: () -> Unit,
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val notificationPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { vm.markNotificationPermissionAsked() }
    LaunchedEffect(vm.enrolled) {
        if (vm.enrolled) {
            ConnectNotifications.syncNow(context)
            if (Build.VERSION.SDK_INT >= 33 && vm.shouldRequestNotificationPermission) {
                notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        }
    }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) {
        vm.setForeground(true)
        if (vm.enrolled && vm.api != null) vm.refreshOnResume()
    }
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) {
        vm.setForeground(false)
    }

    // Loud errors (enrollment, linking) get a dialog, not a status line.
    vm.errorDialog?.let { msg ->
        AlertDialog(
            onDismissRequest = { vm.errorDialog = null },
            confirmButton = {
                TextButton(onClick = { vm.errorDialog = null }) { Text("OK") }
            },
            title = { Text("That didn't work") },
            text = { Text(msg) },
        )
    }

    val api = vm.api
    if (!vm.enrolled) {
        EnrollFlow(vm)
    } else if (api != null) {
        // Coil rides the HiveClient's OkHttp (TOFU trust + auth header).
        val ctx = androidx.compose.ui.platform.LocalContext.current
        val loader = remember(api) {
            ImageLoader.Builder(ctx).okHttpClient(api.http).crossfade(true).build()
        }
        CompositionLocalProvider(LocalImageLoader provides loader) {
            MainScaffold(
                vm,
                notificationDestination,
                notificationCallId,
                notificationAlertKind,
                notificationAlertSubject,
                notificationFoldId,
                onNotificationDestinationConsumed,
            )
        }
    } else {
        // Enrolled but still signing in.
        Surface(Modifier.fillMaxSize(), color = Color.Transparent) {
            Column(
                Modifier.fillMaxSize(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                CircularProgressIndicator(color = Accent)
                Spacer(Modifier.height(16.dp))
                Text(vm.status.ifEmpty { "Signing in…" }, color = TextDim)
            }
        }
    }
}

// ------------------------------------------------------------ enroll/link

@Composable
fun EnrollFlow(vm: ConnectViewModel) {
    var mode by remember { mutableStateOf("welcome") } // welcome|create|signin|link
    var server by remember { mutableStateOf(DefaultHiveServer) }
    var serverAdvanced by remember { mutableStateOf(false) }
    var handle by remember { mutableStateOf("") }
    var invite by remember { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var ageConfirmed by remember { mutableStateOf(false) }

    Surface(Modifier.fillMaxSize(), color = Color.Transparent) {
        Column(
            Modifier
                .fillMaxSize()
                .padding(28.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Text(
                "CONNECT",
                style = BrandTextStyle,
                fontSize = 34.sp,
                fontWeight = FontWeight.Black,
                letterSpacing = 6.sp,
            )
            Text(
                "chronological · ad-free · yours",
                color = TextDim, fontSize = 13.sp,
            )
            Spacer(Modifier.height(36.dp))

            val code = vm.linkCode
            when {
                code != null -> {
                    Text("Approve this phone", color = TextMain, fontSize = 18.sp)
                    Spacer(Modifier.height(12.dp))
                    Text(
                        "On a device that's already signed in, open\nDevices → Link device and enter:",
                        color = TextDim, textAlign = TextAlign.Center, fontSize = 14.sp,
                    )
                    Spacer(Modifier.height(20.dp))
                    Text(
                        code.chunked(4).joinToString(" "),
                        style = BrandTextStyle, fontSize = 34.sp,
                        fontWeight = FontWeight.Bold, letterSpacing = 4.sp,
                    )
                    Spacer(Modifier.height(20.dp))
                    CircularProgressIndicator(color = Accent2, modifier = Modifier.size(28.dp))
                    Spacer(Modifier.height(20.dp))
                    TextButton(onClick = { vm.linkCancel() }) { Text("Cancel", color = TextDim) }
                }

                mode == "welcome" -> {
                    Button(
                        onClick = { mode = "create" },
                        modifier = Modifier.fillMaxWidth().height(52.dp),
                    ) { Text("Create account") }
                    Spacer(Modifier.height(12.dp))
                    OutlinedButton(
                        onClick = { mode = "signin" },
                        modifier = Modifier.fillMaxWidth().height(52.dp),
                    ) { Text("I already have an account") }
                }

                else -> {
                    Surface(
                        modifier = Modifier.fillMaxWidth(),
                        color = PanelHi,
                        shape = MaterialTheme.shapes.medium,
                        tonalElevation = 0.dp,
                    ) {
                        Column(Modifier.padding(horizontal = 14.dp, vertical = 10.dp)) {
                            Text("HIVE", color = TextMuted, fontSize = 11.sp, fontWeight = FontWeight.Bold)
                            Text(server, color = TextMain, fontSize = 14.sp)
                            TextButton(
                                onClick = { serverAdvanced = !serverAdvanced },
                                contentPadding = PaddingValues(0.dp),
                            ) {
                                Text(
                                    if (serverAdvanced) "Hide server settings" else "Advanced server settings",
                                    color = Accent2,
                                )
                            }
                            if (serverAdvanced) {
                                OutlinedTextField(
                                    value = server,
                                    onValueChange = { server = it },
                                    label = { Text("HIVE server") },
                                    singleLine = true,
                                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                                    modifier = Modifier.fillMaxWidth(),
                                )
                            }
                        }
                    }
                    if (mode == "create") {
                        Spacer(Modifier.height(10.dp))
                        OutlinedTextField(
                            value = handle, onValueChange = { handle = it },
                            label = { Text("Handle — anything you like") }, singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(Modifier.height(10.dp))
                        OutlinedTextField(
                            value = password, onValueChange = { password = it },
                            label = { Text("Password — for signing in on new devices") },
                            singleLine = true,
                            visualTransformation = PasswordVisualTransformation(),
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(Modifier.height(10.dp))
                        OutlinedTextField(
                            value = invite, onValueChange = { invite = it },
                            label = { Text("Invite code (if required)") }, singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(Modifier.height(10.dp))
                        // Age gate: account creation requires attestation (18+).
                        Row(
                            Modifier.fillMaxWidth().clickable { ageConfirmed = !ageConfirmed },
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Checkbox(
                                checked = ageConfirmed,
                                onCheckedChange = { ageConfirmed = it },
                            )
                            Text(
                                "I confirm I am 18 years of age or older",
                                color = TextDim, fontSize = 13.sp,
                            )
                        }
                        // Terms agreement: creating the account records
                        // acceptance of the current ToS/PP server-side.
                        var viewingDoc by remember { mutableStateOf<String?>(null) }
                        Row(Modifier.fillMaxWidth().padding(top = 4.dp)) {
                            Text(
                                buildString {
                                    append("By creating an account you agree to the ")
                                },
                                color = TextMuted, fontSize = 12.sp,
                            )
                        }
                        Row(Modifier.fillMaxWidth()) {
                            Text(
                                "Terms of Service",
                                color = Accent2, fontSize = 12.sp,
                                modifier = Modifier.clickable { viewingDoc = "terms" },
                            )
                            Text(" and ", color = TextMuted, fontSize = 12.sp)
                            Text(
                                "Privacy Policy",
                                color = Accent2, fontSize = 12.sp,
                                modifier = Modifier.clickable { viewingDoc = "privacy" },
                            )
                            Text(".", color = TextMuted, fontSize = 12.sp)
                        }
                        viewingDoc?.let { doc ->
                            LegalDocViewerDialog(
                                vm, doc,
                                onClose = { viewingDoc = null },
                                serverOverride = server,
                            )
                        }
                    } else if (mode == "signin") {
                        Spacer(Modifier.height(10.dp))
                        OutlinedTextField(
                            value = handle, onValueChange = { handle = it },
                            label = { Text("Handle") }, singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(Modifier.height(10.dp))
                        OutlinedTextField(
                            value = password, onValueChange = { password = it },
                            label = { Text("Password") }, singleLine = true,
                            visualTransformation = PasswordVisualTransformation(),
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                            modifier = Modifier.fillMaxWidth(),
                        )
                    } else {
                        Spacer(Modifier.height(10.dp))
                        Text(
                            "This phone will show a short code. Approve it from " +
                                "a device that's already signed in — your keys never move.",
                            color = TextDim, fontSize = 13.sp,
                        )
                    }
                    Spacer(Modifier.height(20.dp))
                    Button(
                        onClick = {
                            when (mode) {
                                "create" -> vm.enroll(server, handle, invite, password)
                                "signin" -> vm.signInWithPassword(server, handle, password)
                                else -> vm.linkStart(server)
                            }
                        },
                        enabled = !vm.busy && (mode != "create" || ageConfirmed),
                        modifier = Modifier.fillMaxWidth().height(52.dp),
                    ) {
                        Text(
                            when (mode) {
                                "create" -> "Create account"
                                "signin" -> "Sign in"
                                else -> "Get link code"
                            }
                        )
                    }
                    if (mode == "signin") {
                        TextButton(onClick = { mode = "link" }) {
                            Text("Link from another device instead", color = Accent2)
                        }
                    }
                    TextButton(onClick = { mode = "welcome" }) { Text("Back", color = TextDim) }
                }
            }

            if (vm.status.isNotEmpty()) {
                Spacer(Modifier.height(16.dp))
                Text(vm.status, color = TextDim, fontSize = 13.sp)
            }
        }
    }
}

// -------------------------------------------------------------- main shell

/** Suite-styled bottom-nav item colors: cyan selection on a soft purple pill. */
@Composable
fun navColors() = NavigationBarItemDefaults.colors(
    selectedIconColor = Accent2,
    selectedTextColor = TextMain,
    indicatorColor = AccentSoft,
    unselectedIconColor = TextMuted,
    unselectedTextColor = TextMuted,
)

/** "A new build is on the HIVE" — one tap to download + install in place. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun UpdateBanner(vm: ConnectViewModel) {
    var showNotes by remember { mutableStateOf(false) }
    val major = vm.updateSeverity == "major"
    val bannerColor = if (major) Danger.copy(alpha = 0.22f) else AccentSoft
    val actionColor = if (major) Danger else Accent2
    if (showNotes) {
        AlertDialog(
            onDismissRequest = { showNotes = false },
            containerColor = Panel,
            title = {
                Text(
                    buildString {
                        append(vm.updateTitle)
                        if (vm.updateVersion.isNotBlank()) append(" ${vm.updateVersion}")
                    },
                    color = TextMain,
                )
            },
            text = {
                Column {
                    if (major) {
                        Text(
                            "Major update — review before installing.",
                            color = Danger,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Spacer(Modifier.height(8.dp))
                    }
                    if (vm.updateNotes.isEmpty()) {
                        Text("No release notes were published for this build.", color = TextDim)
                    } else {
                        vm.updateNotes.forEach { note ->
                            Text("• $note", color = TextMain, fontSize = 14.sp)
                            Spacer(Modifier.height(6.dp))
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { showNotes = false; vm.installUpdate() }, enabled = !vm.updateBusy) {
                    Text(if (vm.updateBusy) "Downloading…" else "Update now", color = actionColor)
                }
            },
            dismissButton = {
                TextButton(onClick = { showNotes = false }) { Text("Later", color = TextDim) }
            },
        )
    }
    Column(
        Modifier
            .fillMaxWidth()
            .background(bannerColor)
            .padding(horizontal = 12.dp, vertical = 4.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Text(
                if (major) "Major Nexus Connect update available." else "A new version of Nexus Connect is available.",
                color = TextMain,
                fontSize = 12.sp,
                lineHeight = 15.sp,
                maxLines = 2,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).padding(end = 4.dp),
            )
            TooltipBox(
                positionProvider = TooltipDefaults.rememberPlainTooltipPositionProvider(),
                tooltip = { PlainTooltip { Text("What's changing") } },
                state = rememberTooltipState(),
            ) {
                IconButton(
                    onClick = { showNotes = true },
                    enabled = !vm.updateBusy,
                    modifier = Modifier.size(36.dp),
                ) {
                    Icon(Icons.Outlined.Info, "What's changing", tint = actionColor)
                }
            }
            TooltipBox(
                positionProvider = TooltipDefaults.rememberPlainTooltipPositionProvider(),
                tooltip = { PlainTooltip { Text("Update now") } },
                state = rememberTooltipState(),
            ) {
                IconButton(
                    onClick = { vm.installUpdate() },
                    enabled = !vm.updateBusy,
                    modifier = Modifier.size(36.dp),
                ) {
                    if (vm.updateBusy) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(18.dp),
                            strokeWidth = 2.dp,
                            color = actionColor,
                        )
                    } else {
                        Icon(Icons.Outlined.Download, "Update now", tint = actionColor)
                    }
                }
            }
        }

        if (vm.updateBusy) {
            val total = vm.updateTotalBytes
            val downloaded = vm.updateDownloadedBytes
            val progress = if (total > 0L) (downloaded.toFloat() / total.toFloat()).coerceIn(0f, 1f) else 0f
            Spacer(Modifier.height(4.dp))
            LinearProgressIndicator(
                progress = { progress },
                modifier = Modifier.fillMaxWidth().height(5.dp),
                color = actionColor,
                trackColor = PanelHover,
            )
            Spacer(Modifier.height(4.dp))
            Text(
                buildString {
                    if (total > 0L) append("${fmtBytes(downloaded)} / ${fmtBytes(total)}")
                    else append(fmtBytes(downloaded))
                    append(" · ")
                    append(fmtSpeed(vm.updateBytesPerSecond))
                    append(" · ")
                    append(fmtEta(vm.updateEtaSeconds))
                },
                color = TextDim,
                fontSize = 12.sp,
            )
        }
    }
    HorizontalDivider(color = BorderSoft, thickness = 1.dp)
}

@Composable
fun NotifierInstallBanner(vm: ConnectViewModel) {
    Column(
        Modifier
            .fillMaxWidth()
            .background(Honey.copy(alpha = 0.16f))
            .padding(horizontal = 12.dp, vertical = 7.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Column(Modifier.weight(1f)) {
                Text("Nexus Notify", color = TextMain, fontWeight = FontWeight.SemiBold, fontSize = 13.sp)
                Text(
                    if (vm.notifierInstalled) "Companion update available" else "Companion not installed",
                    color = TextDim,
                    fontSize = 12.sp,
                )
            }
            Button(
                onClick = vm::installNotifier,
                enabled = !vm.notifierBusy,
                colors = ButtonDefaults.buttonColors(containerColor = Honey, contentColor = Bg),
                contentPadding = PaddingValues(horizontal = 12.dp, vertical = 7.dp),
            ) {
                if (vm.notifierBusy) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(16.dp),
                        strokeWidth = 2.dp,
                        color = Bg,
                    )
                } else {
                    Icon(Icons.Outlined.Download, null, modifier = Modifier.size(17.dp))
                    Spacer(Modifier.width(6.dp))
                    Text(if (vm.notifierInstalled) "Update" else "Install")
                }
            }
        }
        if (vm.notifierBusy && vm.notifierTotalBytes > 0L) {
            Spacer(Modifier.height(6.dp))
            LinearProgressIndicator(
                progress = {
                    (vm.notifierDownloadedBytes.toFloat() / vm.notifierTotalBytes.toFloat())
                        .coerceIn(0f, 1f)
                },
                modifier = Modifier.fillMaxWidth().height(4.dp),
                color = Honey,
                trackColor = PanelHover,
            )
        }
    }
}

@Composable
fun MainScaffold(
    vm: ConnectViewModel,
    notificationDestination: String?,
    notificationCallId: String?,
    notificationAlertKind: String?,
    notificationAlertSubject: String?,
    notificationFoldId: String?,
    onNotificationDestinationConsumed: () -> Unit,
) {
    var tab by remember { mutableIntStateOf(0) }
    var tabBackStack by remember { mutableStateOf(listOf<Int>()) }
    var composerOpen by remember { mutableStateOf(false) }
    var explicitlyOpenedCallId by remember { mutableStateOf<String?>(null) } // A user-tapped fallback alert may open its call, unlike an unsolicited incoming invite.
    fun selectTab(next: Int) {
        if (next == tab) return
        tabBackStack = (tabBackStack + tab).takeLast(12)
        tab = next
    }

    LaunchedEffect(
        notificationDestination,
        notificationCallId,
        notificationAlertKind,
        notificationAlertSubject,
        notificationFoldId,
    ) {
        when (notificationDestination) {
            OPEN_ALERTS_ACTION -> {
                vm.dmOpen = false
                selectTab(2)
                vm.markAlertsSeen()
            }
            OPEN_MESSAGES_ACTION -> vm.dmOpen = true
            OPEN_FOLD_ACTION -> vm.openFoldActivity(
                notificationAlertKind.orEmpty(),
                notificationAlertSubject.orEmpty(),
                notificationFoldId.orEmpty(),
            )
            OPEN_CALL_ACTION -> {
                vm.dmOpen = false
                explicitlyOpenedCallId = notificationCallId // Preserve explicit navigation from Notify's fallback notification.
                vm.restorePendingCall(notificationCallId)
            }
        }
        if (notificationDestination != null) onNotificationDestinationConsumed()
    }

    androidx.activity.compose.BackHandler(enabled = tabBackStack.isNotEmpty() || tab != 0) {
        if (tabBackStack.isNotEmpty()) {
            tab = tabBackStack.last()
            tabBackStack = tabBackStack.dropLast(1)
        } else {
            tab = 0
        }
    }

    // §16 legal review gate: updated ToS/PP must be accepted before any
    // other surface is usable. Renders above everything, consumes Back.
    if (vm.legalGateNeeded) {
        LegalGateSheet(vm)
        return
    }

    if (vm.callState.phase == CallPhase.CONNECTING || vm.callState.phase == CallPhase.ACTIVE || // Answered and outgoing calls still show their controls.
        (vm.callState.phase == CallPhase.INCOMING && vm.callState.callId == explicitlyOpenedCallId)) { // An incoming call takes over only after an explicit notification tap.
        CallPane(vm)
        return
    }

    // Viewing someone else's profile overlays everything (back closes it).
    vm.viewedProfile?.let { p ->
        ProfileScreen(vm, p, vm.viewedPosts, own = false, onBack = { vm.closeProfile() })
        // Sheets must still render above the profile overlay.
        vm.openPostId?.let { postId -> CommentsSheet(vm, postId) }
        return
    }

    // Single-post detail (opened from alerts); profile overlay covers it.
    if (vm.detailPost != null) {
        PostDetailPane(vm)
        vm.openPostId?.let { postId -> CommentsSheet(vm, postId) }
        return
    }

    if (vm.supportOpen) {
        SupportPane(vm)
        return
    }

    if (vm.foldsOpen) {
        FoldsPane(vm)
        vm.openPostId?.let { postId -> CommentsSheet(vm, postId) }
        return
    }

    if (vm.dmOpen) {
        DmPane(vm)
        return
    }

    Scaffold(
        containerColor = Color.Transparent,
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        bottomBar = {
            Column {
                HorizontalDivider(color = BorderSoft, thickness = 1.dp)
                NavigationBar(
                    containerColor = Panel.copy(alpha = 0.92f),
                    tonalElevation = 0.dp,
                    windowInsets = WindowInsets(0, 0, 0, 0),
                ) {
                NavigationBarItem(
                    selected = tab == 0, onClick = { selectTab(0) },
                    icon = { Icon(Icons.Outlined.Home, null) }, label = { Text("Home") },
                    colors = navColors(),
                )
                NavigationBarItem(
                    selected = tab == 1, onClick = { selectTab(1) },
                    icon = { Icon(Icons.Outlined.Search, null) }, label = { Text("Discover") },
                    colors = navColors(),
                )
                NavigationBarItem(
                    selected = false, onClick = { composerOpen = true },
                    icon = {
                        Box(
                            Modifier
                                .background(BrandBrush, MaterialTheme.shapes.medium),
                        ) {
                            Text(
                                "+", color = TextMain, fontSize = 22.sp,
                                fontWeight = FontWeight.Bold,
                                modifier = Modifier.padding(horizontal = 14.dp, vertical = 1.dp),
                            )
                        }
                    },
                    label = { Text("Create") },
                    colors = navColors(),
                )
                NavigationBarItem(
                    selected = tab == 2,
                    onClick = { selectTab(2); vm.markAlertsSeen() },
                    icon = {
                        BadgedBox(badge = {
                            if (vm.unseen > 0) {
                                Badge(containerColor = Accent2, contentColor = Bg) {
                                    Text("${vm.unseen}")
                                }
                            }
                        }) { Icon(Icons.Outlined.Notifications, null) }
                    },
                    label = { Text("Alerts") },
                    colors = navColors(),
                )
                NavigationBarItem(
                    selected = tab == 3, onClick = { selectTab(3) },
                    icon = { Avatar(vm.myProfile?.avatarBlob, vm.handle, 26.dp, vm) },
                    label = { Text("Profile") },
                    colors = navColors(),
                )
                }
            }
        },
    ) { pad ->
        Box(Modifier.padding(pad)) {
            Column {
                if (vm.updateAvailable) UpdateBanner(vm)
                if (vm.notifierInstallAvailable) NotifierInstallBanner(vm)
                Box(Modifier.weight(1f)) {
                    when (tab) {
                        0 -> FeedPane(vm)
                        1 -> DiscoverPane(vm)
                        2 -> AlertsPane(vm)
                        3 -> vm.myProfile?.let { p ->
                            ProfileScreen(vm, p, vm.myPosts, own = true, onBack = null)
                        } ?: Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                            CircularProgressIndicator(color = Accent)
                        }
                    }
                }
            }
        }
    }

    if (composerOpen) {
        ComposerSheet(vm, onClose = { composerOpen = false })
    }

    vm.openPostId?.let { postId ->
        CommentsSheet(vm, postId)
    }
}
