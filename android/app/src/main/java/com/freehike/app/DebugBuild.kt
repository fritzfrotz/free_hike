package com.freehike.app

import android.content.Context
import android.content.pm.ApplicationInfo

/**
 * Whether this APK is debuggable (P10.C1) — the one signal that gates
 * per-slice and per-second log lines out of release builds. Capacitor reads
 * the same flag for the web layer (loggingBehavior 'debug'), so web and
 * Kotlin lines go quiet together. AGP sets the flag on the debug build type
 * only.
 */
object DebugBuild {
    fun isDebuggable(context: Context): Boolean =
        (context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0
}
