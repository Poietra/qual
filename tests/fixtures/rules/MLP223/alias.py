import manim as mn


class AliasedStroke(mn.Scene):
    def construct(self):
        circle = mn.Circle(fill_opacity=1, stroke_opacity=0, stroke_width=6)
        self.play(circle.animate.shift(mn.RIGHT), run_time=3)
