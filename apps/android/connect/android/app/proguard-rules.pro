-dontwarn com.google.errorprone.annotations.CanIgnoreReturnValue
-dontwarn com.google.errorprone.annotations.CheckReturnValue
-dontwarn com.google.errorprone.annotations.Immutable
-dontwarn com.google.errorprone.annotations.RestrictedApi

# The WebRTC AAR does not ship consumer rules, but JNI resolves these by name.
-keep class org.webrtc.** { *; }
-keep class org.jni_zero.JniInit { *; }