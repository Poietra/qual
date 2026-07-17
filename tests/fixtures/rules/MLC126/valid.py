from manim import *


def load_sprite():
    return ImageMobject("sprite.png")


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        image = ImageMobject("photo.png")
        vectors = VGroup(square, dot)
        mixed = Group(square, image)
        vectors.add(Circle())
        mixed.add(image)
        vectors.add(load_sprite())
        self.add(square)
