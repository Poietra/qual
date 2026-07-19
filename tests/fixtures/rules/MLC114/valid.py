from manim import FadeOut, RIGHT, Scene, Square, override_animate


class OverrideSquare(Square):
    def clear_shape(self):
        return self.set_opacity(0)

    @override_animate(clear_shape)
    def _clear_shape_animation(self, anim_args=None):
        return FadeOut(self, **(anim_args or {}))


class NoOverrideSquare(OverrideSquare):
    # The nearest method replaces the decorated base method, so it has no
    # inherited override-animation attribute.
    def clear_shape(self):
        return self.set_opacity(0)


class ValidOverrideUse(Scene):
    def construct(self):
        custom = OverrideSquare()
        ordinary = NoOverrideSquare()
        square = Square()
        self.play(custom.animate.clear_shape())
        self.play(ordinary.animate.clear_shape().shift(RIGHT))
        self.play(square.animate.shift(RIGHT).rotate(1))
