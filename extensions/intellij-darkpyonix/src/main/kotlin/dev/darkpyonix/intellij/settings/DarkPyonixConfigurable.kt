package dev.darkpyonix.intellij.settings

import com.intellij.openapi.options.BoundConfigurable
import com.intellij.openapi.ui.DialogPanel
import com.intellij.ui.dsl.builder.bindSelected
import com.intellij.ui.dsl.builder.bindText
import com.intellij.ui.dsl.builder.columns
import com.intellij.ui.dsl.builder.COLUMNS_LARGE
import com.intellij.ui.dsl.builder.panel

class DarkPyonixConfigurable : BoundConfigurable("DarkPyonix") {
    private val s get() = DarkPyonixSettings.getInstance().state

    override fun createPanel(): DialogPanel = panel {
        group("Manager") {
            row("darkpyonix CLI:") {
                textField().columns(COLUMNS_LARGE)
                    .bindText({ s.command ?: "" }, { s.command = it })
                    .comment("Full path if the IDE does not see it on PATH.")
            }
            row("Spawn arguments:") {
                textField().columns(COLUMNS_LARGE)
                    .bindText({ s.spawnArgs ?: "" }, { s.spawnArgs = it })
                    .comment("Used when no manager is registered in ~/.darkpyonix/managers (SPEC FR-C1).")
            }
            row("Dedicated manager URL:") {
                textField().columns(COLUMNS_LARGE)
                    .bindText({ s.managerUrl ?: "" }, { s.managerUrl = it })
                    .comment("Optional. Leave empty to discover a local manager.")
            }
            row("Token:") {
                passwordField().columns(COLUMNS_LARGE)
                    .bindText({ s.managerToken ?: "" }, { s.managerToken = it })
            }
            row("Python for new kernels:") {
                textField().columns(COLUMNS_LARGE)
                    .bindText({ s.python ?: "" }, { s.python = it })
            }
        }
        group("Collaboration") {
            row("Nickname:") {
                textField().bindText({ s.nickname ?: "" }, { s.nickname = it })
                    .comment("Shown to other clients next to your cursor and locks.")
            }
            row {
                checkBox("Attach to a running kernel when a notebook opens")
                    .bindSelected({ s.attachOnOpen }, { s.attachOnOpen = it })
            }
            row {
                checkBox("Queue runs while the kernel is busy")
                    .bindSelected({ s.queueWhenBusy }, { s.queueWhenBusy = it })
            }
        }
    }
}
