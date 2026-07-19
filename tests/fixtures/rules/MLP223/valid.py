from manim import *


class NearMisses(Scene):
    def construct(self):
        zero_width = Circle(stroke_opacity=0, stroke_width=0)
        self.play(zero_width.animate.shift(RIGHT))

        visible_stroke = Circle(stroke_opacity=0.2, stroke_width=8)
        self.play(visible_stroke.animate.shift(RIGHT))

        future_visible = Circle(stroke_opacity=0, stroke_width=8)
        self.play(future_visible.animate.shift(RIGHT))
        future_visible.set_stroke(opacity=1)

        changing = Circle(stroke_opacity=0, stroke_width=8)
        self.play(changing.animate.set_opacity(1))

        changed_earlier = Circle(stroke_opacity=0, stroke_width=8)
        self.play(changed_earlier.animate.set_opacity(1))
        self.play(changed_earlier.animate.shift(RIGHT))

        background_only = Circle(stroke_opacity=1, stroke_width=8)
        background_only.set_stroke(width=8, opacity=0, background=True)
        self.play(background_only.animate.shift(RIGHT))

        cold = Circle(stroke_opacity=0, stroke_width=8)
        self.add(cold)
