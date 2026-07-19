from manim import *


class LongOverlay(Scene):
    def construct(self):
        overlay = FullScreenRectangle(fill_opacity=0.4, stroke_width=0)
        self.add(overlay)
        self.play(overlay.animate.scale(1.01), run_time=6)


class SetterOverlay(Scene):
    def construct(self):
        overlay = FullScreenRectangle().set_fill(opacity=0.25)
        self.play(overlay.animate.scale(1.01), run_time=5)
