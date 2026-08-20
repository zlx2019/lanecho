package io.github.zlx2019.lanecho.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

// Brand palette derived from the desktop app icon (mint clipboard with a pink
// clip and heart): teal-green primary, soft pink secondary. Fixed schemes on
// purpose — the shared brand look wins over wallpaper-based dynamic color.

private val LightScheme = lightColorScheme(
    primary = Color(0xFF206A5E),
    onPrimary = Color(0xFFFFFFFF),
    primaryContainer = Color(0xFFA5F0DF),
    onPrimaryContainer = Color(0xFF00201B),
    secondary = Color(0xFFA84E62),
    onSecondary = Color(0xFFFFFFFF),
    secondaryContainer = Color(0xFFFFD9DF),
    onSecondaryContainer = Color(0xFF40001C),
    tertiary = Color(0xFF456179),
    onTertiary = Color(0xFFFFFFFF),
    tertiaryContainer = Color(0xFFCBE5FF),
    onTertiaryContainer = Color(0xFF001E31),
    background = Color(0xFFF6FBF8),
    onBackground = Color(0xFF171D1B),
    surface = Color(0xFFF6FBF8),
    onSurface = Color(0xFF171D1B),
    surfaceVariant = Color(0xFFDBE5E0),
    onSurfaceVariant = Color(0xFF3F4945),
    outline = Color(0xFF6F7975),
    outlineVariant = Color(0xFFBFC9C4),
)

private val DarkScheme = darkColorScheme(
    primary = Color(0xFF89D3C4),
    onPrimary = Color(0xFF003730),
    primaryContainer = Color(0xFF005046),
    onPrimaryContainer = Color(0xFFA5F0DF),
    secondary = Color(0xFFFFB1C1),
    onSecondary = Color(0xFF650033),
    secondaryContainer = Color(0xFF88374B),
    onSecondaryContainer = Color(0xFFFFD9DF),
    tertiary = Color(0xFFADCAE6),
    onTertiary = Color(0xFF153349),
    tertiaryContainer = Color(0xFF2D4961),
    onTertiaryContainer = Color(0xFFCBE5FF),
    background = Color(0xFF0F1513),
    onBackground = Color(0xFFDEE4E0),
    surface = Color(0xFF0F1513),
    onSurface = Color(0xFFDEE4E0),
    surfaceVariant = Color(0xFF3F4945),
    onSurfaceVariant = Color(0xFFBFC9C4),
    outline = Color(0xFF89938F),
    outlineVariant = Color(0xFF3F4945),
)

/** Material 3 with the lanecho brand palette; light/dark follows the system. */
@Composable
fun LanechoTheme(content: @Composable () -> Unit) {
    val scheme = if (isSystemInDarkTheme()) DarkScheme else LightScheme
    MaterialTheme(colorScheme = scheme, content = content)
}
