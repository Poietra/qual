from manim import *
from manim.renderer.shader import Object3D


class Suppressed(Scene):
    def construct(self):
        node = Object3D()
        self.add(node)  # manim-lint: ignore[MLR123]
        self.wait()
