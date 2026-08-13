package com.quill.decodetest

import android.app.Activity
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Bundle
import android.util.Log
import android.view.SurfaceHolder
import android.view.SurfaceView
import java.io.BufferedInputStream
import java.io.DataInputStream
import java.net.ServerSocket
import java.net.Socket

/**
 * Milestone 3 throwaway test app: listens on a TCP port (reached via
 * `adb forward tcp:PORT tcp:PORT`, matching the design doc's transport
 * direction -- this app listens, the host daemon connects out), reads
 * length-prefixed H.264 frames, and feeds them straight into MediaCodec.
 *
 * Hardcoded 1920x1080 and a fixed port are intentional for this throwaway
 * scope -- the real capability handshake (no hardcoded resolution anywhere)
 * is Milestone 4+ work, not this transport-validation step.
 */
class MainActivity : Activity(), SurfaceHolder.Callback {
    private val tag = "QuillDecodeTest"
    private val port = 7777
    private val width = 1920
    private val height = 1080

    private var decodeThread: Thread? = null

    @Volatile
    private var running = true

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val surfaceView = SurfaceView(this)
        setContentView(surfaceView)
        surfaceView.holder.addCallback(this)
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        decodeThread = Thread { runDecodeLoop(holder) }.also { it.start() }
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        running = false
        decodeThread?.interrupt()
    }

    override fun onDestroy() {
        super.onDestroy()
        running = false
    }

    private fun runDecodeLoop(holder: SurfaceHolder) {
        Log.i(tag, "Listening on port $port, waiting for daemon (adb forward)...")
        var codec: MediaCodec? = null
        try {
            ServerSocket(port).use { server ->
                server.reuseAddress = true
                val socket: Socket = server.accept()
                Log.i(tag, "daemon connected from ${socket.remoteSocketAddress}")
                val input = DataInputStream(BufferedInputStream(socket.getInputStream()))

                val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
                // Diagnostic: force the AOSP software decoder instead of the
                // device's hardware one, to tell apart a hardware-decoder
                // quirk from a bug in our own bitstream/app code.
                codec = MediaCodec.createByCodecName("c2.android.avc.decoder")
                Log.i(tag, "using decoder: ${codec!!.name}")
                codec!!.configure(format, holder.surface, null, 0)
                codec!!.start()

                val bufferInfo = MediaCodec.BufferInfo()
                var frameCount = 0L
                var queuedCount = 0L
                var renderedCount = 0L
                var presentationTimeUs = 0L

                while (running) {
                    val length = try {
                        input.readInt()
                    } catch (e: Exception) {
                        Log.i(tag, "stream ended: ${e.message}")
                        break
                    }
                    if (length <= 0 || length > 16 * 1024 * 1024) {
                        Log.w(tag, "bogus frame length $length, stopping")
                        break
                    }
                    val frameBytes = ByteArray(length)
                    input.readFully(frameBytes)

                    val inIndex = codec!!.dequeueInputBuffer(10_000)
                    if (inIndex >= 0) {
                        val inputBuffer = codec!!.getInputBuffer(inIndex)!!
                        inputBuffer.clear()
                        inputBuffer.put(frameBytes)
                        codec!!.queueInputBuffer(
                            inIndex, 0, frameBytes.size,
                            presentationTimeUs, MediaCodec.BUFFER_FLAG_KEY_FRAME
                        )
                        presentationTimeUs += 16_666 // ~60fps spacing, cosmetic only for v0
                        queuedCount++
                    } else {
                        Log.w(tag, "no input buffer available (dequeueInputBuffer=$inIndex)")
                    }

                    var outIndex = codec!!.dequeueOutputBuffer(bufferInfo, 10_000)
                    while (true) {
                        when {
                            outIndex >= 0 -> {
                                codec!!.releaseOutputBuffer(outIndex, true)
                                renderedCount++
                                outIndex = codec!!.dequeueOutputBuffer(bufferInfo, 0)
                            }
                            outIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                                Log.i(tag, "output format changed: ${codec!!.outputFormat}")
                                outIndex = codec!!.dequeueOutputBuffer(bufferInfo, 0)
                            }
                            outIndex == MediaCodec.INFO_TRY_AGAIN_LATER -> break
                            else -> {
                                Log.w(tag, "dequeueOutputBuffer returned $outIndex")
                                break
                            }
                        }
                    }

                    frameCount++
                    if (frameCount == 1L || frameCount % 30 == 0L) {
                        Log.i(
                            tag,
                            "frame $frameCount ($length bytes): queued=$queuedCount rendered=$renderedCount"
                        )
                    }
                }
            }
        } catch (e: Exception) {
            Log.e(tag, "decode loop error", e)
        } finally {
            codec?.stop()
            codec?.release()
            Log.i(tag, "decode loop stopped")
        }
    }
}
