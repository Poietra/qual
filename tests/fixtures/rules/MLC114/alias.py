import manim as mn


class OverrideSquare(mn.Square):
    def clear_shape(self):
        return self.set_opacity(0)

    @mn.override_animate(clear_shape)
    def _clear_shape_animation(self, anim_args=None):
        return mn.FadeOut(self, **(anim_args or {}))


class AliasOverrideChain(mn.Scene):
    def construct(self):
        custom = OverrideSquare()
        self.play(custom.animate.clear_shape().shift(mn.RIGHT))
