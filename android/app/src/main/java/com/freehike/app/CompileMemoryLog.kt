package com.freehike.app

import android.os.Debug
import java.io.File

/**
 * One memory line per background-compile slice (P9.C11), logged by the
 * worker under its own tag so a device capture yields a curve against the
 * 50 MB dirty-heap budget (ARCHITECTURE.md P3).
 *
 * The figures are the whole app process, not the engine alone: RssAnon is
 * the budget's metric (dirty anonymous pages, manual §3a) but also counts
 * ART and the web view's in-process share; native_kb (malloc'd bytes, the
 * engine's allocations included) is the closest cheap proxy for the
 * engine. The engine reports no dirty-heap figure of its own over FFI.
 *
 * Logging only: [sample] never throws. The formatting is Android-free so
 * the JVM unit test can drive it.
 */
object CompileMemoryLog {

    /** RssAnon in kB from the text of /proc/self/status, or null. */
    fun rssAnonKb(procStatus: String): Long? =
        procStatus.lineSequence()
            .firstOrNull { it.startsWith("RssAnon:") }
            ?.removePrefix("RssAnon:")
            ?.trim()
            ?.removeSuffix("kB")
            ?.trim()
            ?.toLongOrNull()

    fun line(slice: Int, pssKb: Long, rssAnonKb: Long?, nativeKb: Long, javaKb: Long): String =
        "BG mem slice=$slice pss_kb=$pssKb rss_anon_kb=${rssAnonKb ?: "?"} " +
            "native_kb=$nativeKb java_kb=$javaKb"

    /** This process's figures now, or null if any read fails. */
    fun sample(slice: Int): String? = runCatching {
        val runtime = Runtime.getRuntime()
        line(
            slice = slice,
            pssKb = Debug.getPss(),
            rssAnonKb = rssAnonKb(File("/proc/self/status").readText()),
            nativeKb = Debug.getNativeHeapAllocatedSize() / 1024,
            javaKb = (runtime.totalMemory() - runtime.freeMemory()) / 1024,
        )
    }.getOrNull()
}
