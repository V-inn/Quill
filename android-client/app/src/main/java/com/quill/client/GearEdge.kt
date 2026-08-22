package com.quill.client

/** Which screen edge the settings gear is parked against. */
enum class GearEdge {
    LEFT, TOP, RIGHT, BOTTOM;

    companion object {
        fun fromOrdinal(value: Int): GearEdge =
            entries.getOrElse(value) { RIGHT }
    }
}
