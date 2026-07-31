package quality.kotlinfixture

import quality.kotlinfixture.support.formatLabel

/** Coordinates the Kotlin fixture. */
class KotlinWorker : BaseWorker(), Runnable {
    public val onReady: () -> Unit = { reportReady() }
    private val label: String = "kotlin".asWorkerLabel()

    override fun run() {
        onReady()
    }

    private fun reportReady() {}
}

open class BaseWorker

interface Runnable {
    fun run()
}

enum class KotlinStatus {
    Ready,
    Stopped,
}

fun String.asWorkerLabel(): String = formatLabel(this)

fun main() {
    KotlinWorker().run()
}
