import manim as mn


class AliasedSurface(mn.ThreeDScene):
    def construct(self):
        surface = mn.Surface(lambda u, v: (u, v, 0), resolution=40)
        self.play(surface.animate.shift(mn.RIGHT), run_time=2)
