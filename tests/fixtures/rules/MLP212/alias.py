import manim as mn


class AliasedOverlay(mn.Scene):
    def construct(self):
        overlay = mn.FullScreenRectangle(fill_opacity=0.35, stroke_width=0)
        self.add(overlay)
        self.play(overlay.animate.scale(1.01), run_time=5)
