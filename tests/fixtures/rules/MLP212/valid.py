from manim import *


class NearMisses(Scene):
    def construct(self):
        short = FullScreenRectangle(fill_opacity=0.4)
        self.play(short.animate.scale(1.01), run_time=2)

        opaque = FullScreenRectangle(fill_opacity=1)
        self.play(opaque.animate.scale(1.01), run_time=6)

        changing = FullScreenRectangle(fill_opacity=0.4)
        self.play(changing.animate.set_opacity(1), run_time=6)

        small = Rectangle(fill_opacity=0.4)
        self.play(small.animate.scale(1.01), run_time=6)

        custom_aspect = FullScreenRectangle(aspect_ratio=1, fill_opacity=0.4)
        self.play(custom_aspect.animate.scale(1.01), run_time=6)

        opacity = self.camera.background_opacity
        unknown = FullScreenRectangle(fill_opacity=opacity)
        self.play(unknown.animate.scale(1.01), run_time=6)
