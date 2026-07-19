from manim import *


class BranchOnly(Scene):
    def construct(self, animate_layer):
        overlay = FullScreenRectangle(fill_opacity=0.4)
        if animate_layer:
            self.play(overlay.animate.scale(1.01), run_time=6)
