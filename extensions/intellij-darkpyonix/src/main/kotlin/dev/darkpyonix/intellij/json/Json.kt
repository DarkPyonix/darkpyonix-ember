package dev.darkpyonix.intellij.json

/**
 * A small, dependency-free JSON reader and writer.
 *
 * Values map to Kotlin as: object -> [Map] (insertion ordered), array -> [List], string -> [String],
 * integral number -> [Long], other number -> [Double], true/false -> [Boolean], null -> `null`.
 * The plugin keeps its own codec so the protocol code (and its unit tests) need nothing from
 * the IDE's classpath.
 */
object Json {
    class ParseException(message: String) : IllegalArgumentException(message)

    fun parse(text: String): Any? {
        val p = Parser(text)
        p.skipWs()
        val v = p.value()
        p.skipWs()
        if (!p.atEnd()) throw ParseException("trailing data at ${p.pos}")
        return v
    }

    /** [parse], or `null` when [text] is not valid JSON. */
    fun parseOrNull(text: String): Any? = try {
        parse(text)
    } catch (_: ParseException) {
        null
    }

    fun stringify(value: Any?): String = StringBuilder().also { write(it, value) }.toString()

    private fun write(sb: StringBuilder, value: Any?) {
        when (value) {
            null -> sb.append("null")
            is String -> quote(sb, value)
            is Boolean -> sb.append(value)
            is Int, is Long, is Short, is Byte -> sb.append(value.toString())
            is Double -> if (value.isFinite() && value == Math.rint(value) && Math.abs(value) < 1e15) {
                sb.append(value.toLong())
            } else if (value.isFinite()) {
                sb.append(value)
            } else {
                sb.append("null")
            }
            is Float -> write(sb, value.toDouble())
            is Number -> sb.append(value.toString())
            is Map<*, *> -> {
                sb.append('{')
                var first = true
                for ((k, v) in value) {
                    if (!first) sb.append(',')
                    first = false
                    quote(sb, k.toString())
                    sb.append(':')
                    write(sb, v)
                }
                sb.append('}')
            }
            is Iterable<*> -> {
                sb.append('[')
                var first = true
                for (v in value) {
                    if (!first) sb.append(',')
                    first = false
                    write(sb, v)
                }
                sb.append(']')
            }
            is Array<*> -> write(sb, value.toList())
            else -> quote(sb, value.toString())
        }
    }

    private fun quote(sb: StringBuilder, s: String) {
        sb.append('"')
        for (c in s) {
            when (c) {
                '"' -> sb.append("\\\"")
                '\\' -> sb.append("\\\\")
                '\n' -> sb.append("\\n")
                '\r' -> sb.append("\\r")
                '\t' -> sb.append("\\t")
                '\b' -> sb.append("\\b")
                '\u000C' -> sb.append("\\f")
                else -> if (c < ' ' || c == ' ' || c == ' ') {
                    sb.append("\\u").append(String.format("%04x", c.code))
                } else {
                    sb.append(c)
                }
            }
        }
        sb.append('"')
    }

    private class Parser(val s: String) {
        var pos = 0

        fun atEnd() = pos >= s.length

        fun skipWs() {
            while (pos < s.length && (s[pos] == ' ' || s[pos] == '\t' || s[pos] == '\n' || s[pos] == '\r')) pos++
        }

        fun value(): Any? {
            if (atEnd()) throw ParseException("unexpected end of input")
            return when (val c = s[pos]) {
                '{' -> obj()
                '[' -> arr()
                '"' -> str()
                't' -> literal("true", true)
                'f' -> literal("false", false)
                'n' -> literal("null", null)
                else -> if (c == '-' || c in '0'..'9') num() else throw ParseException("unexpected '$c' at $pos")
            }
        }

        private fun literal(word: String, v: Any?): Any? {
            if (!s.startsWith(word, pos)) throw ParseException("bad literal at $pos")
            pos += word.length
            return v
        }

        private fun obj(): Map<String, Any?> {
            pos++ // {
            val m = LinkedHashMap<String, Any?>()
            skipWs()
            if (!atEnd() && s[pos] == '}') {
                pos++
                return m
            }
            while (true) {
                skipWs()
                if (atEnd() || s[pos] != '"') throw ParseException("expected key at $pos")
                val k = str()
                skipWs()
                if (atEnd() || s[pos] != ':') throw ParseException("expected ':' at $pos")
                pos++
                skipWs()
                m[k] = value()
                skipWs()
                if (atEnd()) throw ParseException("unterminated object")
                when (s[pos]) {
                    ',' -> pos++
                    '}' -> {
                        pos++
                        return m
                    }
                    else -> throw ParseException("expected ',' or '}' at $pos")
                }
            }
        }

        private fun arr(): List<Any?> {
            pos++ // [
            val l = ArrayList<Any?>()
            skipWs()
            if (!atEnd() && s[pos] == ']') {
                pos++
                return l
            }
            while (true) {
                skipWs()
                l.add(value())
                skipWs()
                if (atEnd()) throw ParseException("unterminated array")
                when (s[pos]) {
                    ',' -> pos++
                    ']' -> {
                        pos++
                        return l
                    }
                    else -> throw ParseException("expected ',' or ']' at $pos")
                }
            }
        }

        private fun str(): String {
            pos++ // opening quote
            val sb = StringBuilder()
            while (true) {
                if (atEnd()) throw ParseException("unterminated string")
                val c = s[pos++]
                when {
                    c == '"' -> return sb.toString()
                    c == '\\' -> {
                        if (atEnd()) throw ParseException("bad escape")
                        when (val e = s[pos++]) {
                            '"' -> sb.append('"')
                            '\\' -> sb.append('\\')
                            '/' -> sb.append('/')
                            'b' -> sb.append('\b')
                            'f' -> sb.append('\u000C')
                            'n' -> sb.append('\n')
                            'r' -> sb.append('\r')
                            't' -> sb.append('\t')
                            'u' -> {
                                if (pos + 4 > s.length) throw ParseException("bad \\u escape")
                                val hex = s.substring(pos, pos + 4)
                                sb.append(hex.toIntOrNull(16)?.toChar() ?: throw ParseException("bad \\u escape"))
                                pos += 4
                            }
                            else -> throw ParseException("bad escape '\\$e'")
                        }
                    }
                    c < ' ' -> throw ParseException("control character in string at ${pos - 1}")
                    else -> sb.append(c)
                }
            }
        }

        private fun num(): Any {
            val start = pos
            if (s[pos] == '-') pos++
            if (atEnd()) throw ParseException("bad number")
            if (s[pos] == '0') {
                pos++
            } else if (s[pos] in '1'..'9') {
                while (!atEnd() && s[pos] in '0'..'9') pos++
            } else {
                throw ParseException("bad number at $start")
            }
            var integral = true
            if (!atEnd() && s[pos] == '.') {
                integral = false
                pos++
                if (atEnd() || s[pos] !in '0'..'9') throw ParseException("bad fraction at $start")
                while (!atEnd() && s[pos] in '0'..'9') pos++
            }
            if (!atEnd() && (s[pos] == 'e' || s[pos] == 'E')) {
                integral = false
                pos++
                if (!atEnd() && (s[pos] == '+' || s[pos] == '-')) pos++
                if (atEnd() || s[pos] !in '0'..'9') throw ParseException("bad exponent at $start")
                while (!atEnd() && s[pos] in '0'..'9') pos++
            }
            val text = s.substring(start, pos)
            if (integral) text.toLongOrNull()?.let { return it }
            return text.toDouble()
        }
    }
}

// Typed accessors for decoded JSON objects.

@Suppress("UNCHECKED_CAST")
fun Any?.jsonObject(): Map<String, Any?>? = this as? Map<String, Any?>

fun Any?.jsonList(): List<Any?>? = this as? List<Any?>

fun Map<String, Any?>.str(key: String): String? = this[key] as? String

fun Map<String, Any?>.long(key: String): Long? = (this[key] as? Number)?.toLong()

fun Map<String, Any?>.int(key: String): Int? = (this[key] as? Number)?.toInt()

fun Map<String, Any?>.double(key: String): Double? = (this[key] as? Number)?.toDouble()

fun Map<String, Any?>.bool(key: String): Boolean? = this[key] as? Boolean

fun Map<String, Any?>.obj(key: String): Map<String, Any?>? = this[key].jsonObject()

fun Map<String, Any?>.list(key: String): List<Any?>? = this[key].jsonList()
