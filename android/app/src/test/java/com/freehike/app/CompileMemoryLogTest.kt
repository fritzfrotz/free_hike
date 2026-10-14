package com.freehike.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * P9.C11 — one memory line per background-compile slice, so a device
 * capture yields a curve against the 50 MB dirty-heap budget (P3). RssAnon
 * is the budget's own metric on Linux (manual §3a).
 */
class CompileMemoryLogTest {

    @Test
    fun `rssAnonKb reads RssAnon from proc status and is null when absent`() {
        val status = """
            Name:	com.freehike.app
            VmRSS:	  412345 kB
            RssAnon:	  187654 kB
            RssFile:	  220000 kB
        """.trimIndent()
        assertEquals(187654L, CompileMemoryLog.rssAnonKb(status))
        assertNull(CompileMemoryLog.rssAnonKb("Name:\tx\nVmRSS:\t1 kB\n"))
    }

    @Test
    fun `memory line carries slice, pss, rss_anon, native and java heap`() {
        val line = CompileMemoryLog.line(
            slice = 12,
            pssKb = 301234,
            rssAnonKb = 187654,
            nativeKb = 41000,
            javaKb = 9000,
        )
        assertEquals(
            "BG mem slice=12 pss_kb=301234 rss_anon_kb=187654 native_kb=41000 java_kb=9000",
            line,
        )
        assertEquals(
            "BG mem slice=3 pss_kb=1 rss_anon_kb=? native_kb=2 java_kb=3",
            CompileMemoryLog.line(slice = 3, pssKb = 1, rssAnonKb = null, nativeKb = 2, javaKb = 3),
        )
    }
}
