from manim import FadeOut, RIGHT, Scene, Square, override_animate


class OverrideSquare(Square):
    def clear_shape(self):
        return self.set_opacity(0)

    @override_animate(clear_shape)
    def _clear_shape_animation(self, anim_args=None):
        return FadeOut(self, **(anim_args or {}))


class SuppressedOverrideChain(Scene):
    def construct(self):
        custom = OverrideSquare()
        self.play(custom.animate.clear_shape().shift(RIGHT))  # qual: ignore[MLC114]
