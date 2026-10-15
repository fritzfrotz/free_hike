package com.freehike.app

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * P9.C8 — the background worker's progress sink. It runs inside Rust's FFI
 * callback mid-slice: anything it throws would come back out of
 * compileChunk and fail (and purge) the job, so the body is guarded.
 */
class CompileProgressSinkTest {

    @Test
    fun `sink never throws into the callback`() {
        // Every collaborator throws; onProgress must still return normally.
        val failingSave = CompileProgressSink(
            jobId = "job",
            clock = { 0L },
            save = { throw IllegalStateException("prefs write failed") },
            log = { throw RuntimeException("log failed") },
            verbose = true,
        )
        failingSave.onProgress(12.3f, "pass1")
        failingSave.onProgress(13.0f, "pass1")

        val failingClock = CompileProgressSink(
            jobId = "job",
            clock = { throw Error("clock failed") },
            save = {},
            log = {},
            verbose = false,
        )
        failingClock.onProgress(1.0f, "pass1")
    }

    @Test
    fun `saves on integer change or after the 5 s heartbeat, never otherwise`() {
        var now = 1_000L
        val saved = mutableListOf<CompileProgress>()
        val sink = CompileProgressSink(
            jobId = "job",
            clock = { now },
            save = { saved += it },
            log = {},
            verbose = true,
        )

        sink.onProgress(1.2f, "a")   // first callback: save
        now += 2_000
        sink.onProgress(1.8f, "a")   // same integer, 2 s: no
        now += 2_000
        sink.onProgress(1.9f, "a")   // same integer, 4 s: no
        now += 1_000
        sink.onProgress(1.95f, "a")  // same integer, 5 s: heartbeat save
        now += 1_000
        sink.onProgress(2.0f, "b")   // integer change after 1 s: save
        now += 4_999
        sink.onProgress(2.5f, "b")   // same integer, 4.999 s: no

        assertEquals(listOf(1, 1, 2), saved.map { it.percent })
        assertEquals(listOf(1_000L, 6_000L, 7_000L), saved.map { it.atMs })
        assertEquals(listOf("a", "a", "b"), saved.map { it.status })
        assertEquals(setOf("job"), saved.map { it.jobId }.toSet())
    }

    @Test
    fun `release logs one line per integer percent, debug one per callback`() {
        var now = 0L
        fun run(verbose: Boolean): List<String> {
            val lines = mutableListOf<String>()
            val sink = CompileProgressSink(
                jobId = "job",
                clock = { now },
                save = {},
                log = { lines += it },
                verbose = verbose,
            )
            now = 0L
            sink.onProgress(0.0f, "a")   // first callback
            sink.onProgress(0.7f, "a")   // same integer
            now += 6_000
            sink.onProgress(0.9f, "a")   // same integer, heartbeat save: not a log line
            sink.onProgress(1.0f, "b")   // integer change
            sink.onProgress(1.5f, "b")   // same integer
            sink.onProgress(4.2f, "c")   // integer change (skips 2, 3)
            return lines
        }

        assertEquals(
            listOf("BG compile 0% — a", "BG compile 1% — b", "BG compile 4% — c"),
            run(verbose = false),
        )
        assertEquals(6, run(verbose = true).size)
    }
}
