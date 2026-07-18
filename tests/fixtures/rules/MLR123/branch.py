from manim import *
from manim.mobject.opengl.opengl_surface import OpenGLSurface


def flag():
    return True


class Branchy(Scene):
    def construct(self):
        maybe = OpenGLSurface(lambda u, v: (u, v, 0))
        if flag():
            # Branch-dependent add: a Maybe fact must stay silent.
            self.add(maybe)
        self.wait()
