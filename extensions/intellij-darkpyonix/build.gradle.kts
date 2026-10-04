import org.jetbrains.intellij.platform.gradle.IntelliJPlatformType
import org.jetbrains.intellij.platform.gradle.TestFrameworkType

plugins {
    id("org.jetbrains.kotlin.jvm")
    id("org.jetbrains.intellij.platform")
}

group = providers.gradleProperty("pluginGroup").get()
version = providers.gradleProperty("pluginVersion").get()

kotlin {
    jvmToolchain(21)
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    // The platform test runtime expects opentest4j on the classpath (IJPL-157292).
    testImplementation("org.opentest4j:opentest4j:1.3.0")

    intellijPlatform {
        intellijIdea(providers.gradleProperty("platformVersion").get())
        testFramework(TestFrameworkType.Platform)
    }
}

intellijPlatform {
    pluginConfiguration {
        id = "dev.darkpyonix.intellij"
        name = "DarkPyonix Notebooks"
        version = providers.gradleProperty("pluginVersion")
        ideaVersion {
            sinceBuild = providers.gradleProperty("pluginSinceBuild")
            untilBuild = provider { null }
        }
    }
}

// `./gradlew runIde` starts IntelliJ IDEA (platformVersion); `./gradlew runPyCharm` starts
// PyCharm, where `.pynb` is also registered as a Python file.
intellijPlatformTesting {
    runIde {
        register("runPyCharm") {
            type = IntelliJPlatformType.PyCharmCommunity
            version = providers.gradleProperty("pyCharmVersion")
        }
    }
}

tasks {
    test {
        useJUnit()
    }
}
